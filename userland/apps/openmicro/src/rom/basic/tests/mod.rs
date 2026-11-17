//! BASIC against BASIC II as seen from outside: lines typed at the keyboard
//! of a machine running the clean-room MOS, and what the screen and memory
//! show, as BASIC II showed them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::build;
use crate::machine::bbc::Machine;
use crate::scenario::{mode7_text, type_text, wait};

mod assembler;
mod control;
mod editor;
mod numbers;
mod output;
mod tokenise;
mod values;

/// How long the machine takes to reach BASIC's first prompt.
const BOOT: u64 = 150;
/// A prompt and the first of a typed line's characters on its screen line.
const PROMPTED: usize = 39;

/// A machine with the clean-room MOS and BASIC, at the first prompt.
fn basic() -> Machine {
    let os = crate::rom::mos::model_b().expect("the MOS assembles");
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
fn the_pins_lie_past_every_routine_a_program_runs() {
    // Code flowing into a pin hops over it, which a loop would pay each time.
    let image = build().expect("BASIC assembles");
    let editor = image.symbol("cold").expect("the editor is placed");
    for routine in [
        "statement",
        "statement_next",
        "call_routine",
        "statement_vdu",
        "channel",
        "print_next",
        "expr",
        "factor_token",
        "find_name",
        "fac_multiply",
        "read_number",
        "rnd_argument",
    ] {
        let at = image.symbol(routine).expect("the routine is placed");
        assert!(
            at < editor,
            "{routine} at &{at:04X} lies past the editor at &{editor:04X}"
        );
    }
    for pin in [
        super::RND_ARGUMENT,
        super::RND_NEXT,
        super::ERROR_ENTRY,
        super::ERROR_FREEZE,
    ] {
        assert!(
            editor <= pin,
            "the pin at &{pin:04X} lies before the editor at &{editor:04X}"
        );
    }
}

/// The centiseconds a program typed as `lines`, which ends by printing how
/// long its loop took, printed.
fn printed_time(lines: &[&str], centiseconds: u64) -> u32 {
    after(lines, centiseconds)
        .iter()
        .rev()
        .find_map(|row| row.trim().parse().ok())
        .expect("the loop prints its time")
}

/// The regression: integer statements took a fifth to a quarter longer
/// than BASIC II's, so a program pacing itself against TIME, as Mine
/// Sweeper's setup does, ran out of it. BASIC II takes this loop 282
/// centiseconds.
#[test]
fn an_integer_loop_keeps_basic_ii_s_pace() {
    let taken = printed_time(
        &[
            "10 B%=5:C%=3:TIME=0:FORK%=1TO2000:A%=B%+C%:IFA%=9C%=1",
            "20 NEXT:PRINTTIME",
            "RUN",
        ],
        400,
    );
    assert!(taken <= 282, "the loop took {taken} centiseconds");
}

/// The regression: DIV took a step for each of a dividend's 16 bits
/// however small it was; BASIC II takes this loop 300 centiseconds.
#[test]
fn integer_division_keeps_basic_ii_s_pace() {
    let taken = printed_time(
        &[
            "10 B%=5:TIME=0:FORK%=1TO2000:A%=B%DIV3:NEXT:PRINTTIME",
            "RUN",
        ],
        600,
    );
    assert!(taken <= 300, "the loop took {taken} centiseconds");
}

/// The regression: a multiplier below 256 took sixteen steps; BASIC II
/// takes this loop, a screen address's sum, 371 centiseconds.
#[test]
fn integer_multiplication_keeps_basic_ii_s_pace() {
    let taken = printed_time(
        &[
            "10 B%=5:TIME=0:FORK%=1TO2000:A%=B%*40+B%*8:NEXT:PRINTTIME",
            "RUN",
        ],
        600,
    );
    assert!(taken <= 371, "the loop took {taken} centiseconds");
}

/// The centiseconds 2000 calls of a PROC named `name` take.
fn procedure_calls(name: &str) -> u32 {
    printed_time(
        &[
            &format!("10 TIME=0:FORK%=1TO2000:PROC{name}:NEXT:PRINTTIME"),
            "20 END",
            &format!("30 DEFPROC{name}:ENDPROC"),
            "RUN",
        ],
        400,
    )
}

/// The regression: a call took 58% longer than BASIC II's, which takes this
/// loop 138 centiseconds; a call without actuals stacked what only actuals
/// need, so this one is held to 120.
#[test]
fn a_procedure_call_keeps_its_pace() {
    let taken = procedure_calls("X");
    assert!(taken <= 120, "the loop took {taken} centiseconds");
}

/// The regression: a call read its name through to find its length before
/// comparing it, 54 cycles a character as BASIC II's call costs; it is
/// compared as it is read.
#[test]
fn a_procedure_s_name_is_read_once() {
    let long = procedure_calls("MOVEPLAYER");
    let short = procedure_calls("X");
    // 2000 calls a loop, a centisecond 20000 cycles, nine more characters.
    let per_character = (long - short) * 10 / 9;
    assert!(
        per_character <= 25,
        "each character cost {per_character} cycles"
    );
}

/// The regression: each of a real FOR's three integers made a real took
/// its shifts a bit at a time, all five of its bytes in each; BASIC II
/// takes this loop 214 centiseconds.
#[test]
fn a_real_for_loop_keeps_basic_ii_s_pace() {
    let taken = printed_time(
        &[
            "10 TIME=0:FORK%=1TO1000:FORT=1TO1:NEXT:NEXT:PRINTTIME",
            "RUN",
        ],
        400,
    );
    assert!(taken <= 214, "the loop took {taken} centiseconds");
}

#[test]
fn it_starts_at_the_prompt() {
    let machine = basic();
    let screen = mode7_text(&machine);
    assert_eq!(screen[..6], ["", "OpenMOS Micro 32K", "", "BASIC", "", ">"]);
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
