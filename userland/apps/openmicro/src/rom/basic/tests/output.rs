//! Input and output: PRINT's forms, INPUT's, and the machine's
//! statements.

use super::{assert_screen, basic};
use crate::machine::bbc::keyboard::Key;
use crate::machine::bbc::Machine;
use crate::media::tape::Tape;
use crate::scenario::{mode7_text, type_text, wait};

#[test]
fn print_separators_and_hex() {
    assert_screen(
        &[
            "PRINT ~10,10;\"|\";",
            "PRINT \"1234567890\",1",
            "PRINT 1;2;\"A\";3,4",
            "PRINT 1,:PRINT \"X\"",
            "PRINT \"AB\",:PRINT \"C\"",
            "PRINT \"AB\",;\"C\"",
            "PRINT ~10;10,~-1,255",
            "PRINT ~10 10",
        ],
        50,
        &[
            ">PRINT ~10,10;\"|\";",
            "         A        10|>PRINT \"1234567890\"",
            ",1",
            "1234567890         1",
            ">PRINT 1;2;\"A\";3,4",
            "         12A3                4",
            ">PRINT 1,:PRINT \"X\"",
            "         1",
            "X",
            ">PRINT \"AB\",:PRINT \"C\"",
            "AB",
            "C",
            ">PRINT \"AB\",;\"C\"",
            "AB        C",
            ">PRINT ~10;10,~-1,255",
            "         A10          FFFFFFFF       255",
            "",
            ">PRINT ~10 10",
            "         A         A",
            ">",
        ],
    );
}

#[test]
fn tab_and_statements_that_are_mistakes() {
    assert_screen(
        &[
            "A",
            "A=",
            "A=1:B",
            "10 PRINT\"X\";",
            "RUN",
            "PRINT TAB(3);\"A\"'TAB(1);\"B\"",
            "PRINT TAB(5):PRINT\"Q\"",
        ],
        40,
        &[
            ">A",
            "",
            "Mistake",
            ">A=",
            "",
            "No such variable",
            ">A=1:B",
            "",
            "Mistake",
            ">10 PRINT\"X\";",
            ">RUN",
            "X>PRINT TAB(3);\"A\"'TAB(1);\"B\"",
            "   A",
            " B",
            ">PRINT TAB(5):PRINT\"Q\"",
            "",
            "Q",
            ">",
        ],
    );
}

#[test]
fn width_count_and_pos() {
    assert_screen(
        &[
            "WIDTH 5:PRINT \"ABCDEFGHIJKL\":PRINT 1,2:WIDTH 0",
            "VDU 65:PRINT COUNT;POS",
            "PRINT \"AB\";COUNT;\"C\";COUNT",
            "PRINT CHR$(13);\"XY\";COUNT",
        ],
        40,
        &[
            ">WIDTH 5:PRINT \"ABCDEFGHIJKL\":PRINT 1,2:",
            "WIDTH 0",
            "ABCDE",
            "FGHIJ",
            "KL",
            "",
            "    1",
            "",
            "",
            "    2",
            ">VDU 65:PRINT COUNT;POS",
            "A         011",
            ">PRINT \"AB\";COUNT;\"C\";COUNT",
            "AB2C4",
            ">PRINT CHR$(13);\"XY\";COUNT",
            "XY2",
            ">",
        ],
    );
}

#[test]
fn input_reads_items_and_lines() {
    assert_screen(
        &[
            "INPUT A,B$:PRINT A;\"|\";B$;\"|\"",
            "3*2, XY Z",
            "INPUT \"N\",A:PRINT A",
            "5",
            "INPUT \"Q\";B$,C:PRINT B$;\"|\";C",
            "  HI",
            "7",
            "INPUT LINE A$:PRINT A$;\"|\"",
            " A,B  C",
            "INPUT A$:PRINT A$;\"|\"",
            "\"Q,R\" ",
        ],
        60,
        &[
            ">INPUT A,B$:PRINT A;\"|\";B$;\"|\"",
            "?3*2, XY Z",
            "         3|XY Z|",
            ">INPUT \"N\",A:PRINT A",
            "N?5",
            "         5",
            ">INPUT \"Q\";B$,C:PRINT B$;\"|\";C",
            "Q?  HI",
            "?7",
            "HI|7",
            ">INPUT LINE A$:PRINT A$;\"|\"",
            "? A,B  C",
            " A,B  C|",
            ">INPUT A$:PRINT A$;\"|\"",
            "?\"Q,R\"",
            "Q,R|",
            ">",
        ],
    );
}

#[test]
fn input_prompts() {
    assert_screen(
        &[
            "INPUT \"A\" X:PRINT X",
            "1",
            "INPUT \"B\",X:PRINT X",
            "2",
            "INPUT \"C\";X:PRINT X",
            "3",
            "INPUT X,\"D\" Y:PRINT X;Y",
            "4",
            "5",
            "INPUT \"E\"'X",
            "6",
        ],
        60,
        &[
            ">INPUT \"A\" X:PRINT X",
            "A1",
            "         1",
            ">INPUT \"B\",X:PRINT X",
            "B?2",
            "         2",
            ">INPUT \"C\";X:PRINT X",
            "C?3",
            "         3",
            ">INPUT X,\"D\" Y:PRINT X;Y",
            "?4",
            "D5",
            "         45",
            ">INPUT \"E\"'X",
            "E",
            "6",
            ">",
        ],
    );
}

#[test]
fn machine_statements() {
    assert_screen(
        &[
            "VDU 65,66;67|",
            "TIME=100:PRINT TIME DIV 100",
            "SOUND 1,-15,100,5:ENVELOPE 1,1,2,3,4,5,6,7,8,9,10,11,12,13",
            "OSCLI \"FX 0\"",
            "WIDTH 5:PRINT \"ABCDEFG\":WIDTH 0",
            "A=PAGE:PAGE=&1900:PRINT ~PAGE:PAGE=A",
            "HIMEM=&7000:PRINT ~HIMEM:CLEAR",
        ],
        60,
        &[
            ">VDU 65,66;67|",
            "ABC",
            "No such variable",
            ">TIME=100:PRINT TIME DIV 100",
            "         1",
            ">SOUND 1,-15,100,5:ENVELOPE 1,1,2,3,4,5,",
            "6,7,8,9,10,11,12,13",
            ">OSCLI \"FX 0\"",
            "",
            "OS 1.20",
            ">WIDTH 5:PRINT \"ABCDEFG\":WIDTH 0",
            "ABCDE",
            "FG",
            ">A=PAGE:PAGE=&1900:PRINT ~PAGE:PAGE=A",
            "      1900",
            ">HIMEM=&7000:PRINT ~HIMEM:CLEAR",
            "      7000",
            ">",
        ],
    );
}

#[test]
fn call_passes_its_parameter_block() {
    assert_screen(
        &[
            "10 DIM P% 20:A%=0:B=1.5:C$=\"Z\"",
            "20 ?P%=&60:CALL P%,A%,B,C$,?&70,!&71,$&3000",
            "25 FOR I%=0 TO 21:?(&3100+I%)=?(&600+I%):NEXT",
            "30 Q$=\"\":FOR I%=0 TO 21:Q$=Q$+STR$~?(&3100+I%)+\" \":NEXT:PRINT Q$:PRINT ~P%",
            "RUN",
        ],
        60,
        &[
            ">10 DIM P% 20:A%=0:B=1.5:C$=\"Z\"",
            ">20 ?P%=&60:CALL P%,A%,B,C$,?&70,!&71,$&",
            "3000",
            ">25 FOR I%=0 TO 21:?(&3100+I%)=?(&600+I%",
            "):NEXT",
            ">30 Q$=\"\":FOR I%=0 TO 21:Q$=Q$+STR$~?(&3",
            "100+I%)+\" \":NEXT:PRINT Q$:PRINT ~P%",
            ">RUN",
            "6 4 4 4 C6 E 5 CF E 81 70 0 0 71 0 4 0 3",
            "0 80 0 0 0",
            "       EAE",
            ">",
        ],
    );
}

/// Long enough for a short program's record or play at 1200 baud.
const TAPE: u64 = 1500;

/// A machine at the prompt with `tape` in its recorder, wound to the start,
/// after typing `line` and letting the tape run.
fn with_tape(tape: Tape, line: &str) -> Machine {
    let mut machine = basic();
    machine.insert_tape(tape);
    machine.wind_tape(0);
    type_text(&mut machine, line).expect("a key types each character");
    type_text(&mut machine, "\n").expect("a key types each character");
    wait(&mut machine, TAPE);
    machine
}

#[test]
fn save_load_and_chain_keep_the_program_on_tape() {
    let mut saving = basic();
    saving.insert_tape(Tape::default());
    saving.set_tape_record(true);
    for line in ["10PRINT\"CHAINED\"", "SAVE\"PROG\""] {
        type_text(&mut saving, line).expect("a key types each character");
        type_text(&mut saving, "\n").expect("a key types each character");
    }
    saving.set_key(Key::RETURN, true);
    wait(&mut saving, TAPE);
    saving.set_key(Key::RETURN, false);
    let tape = saving.eject_tape().expect("the tape saved to");

    let mut loading = with_tape(tape.clone(), "LOAD\"PROG\"");
    type_text(&mut loading, "LIST\n").expect("a key types each character");
    wait(&mut loading, 50);
    let screen = mode7_text(&loading);
    assert!(
        screen.iter().any(|line| line == "   10PRINT\"CHAINED\""),
        "LOAD took the program: {screen:?}"
    );

    let chained = mode7_text(&with_tape(tape, "CHAIN\"PROG\""));
    assert!(
        chained.iter().any(|line| line == "CHAINED"),
        "CHAIN ran it: {chained:?}"
    );
}

#[test]
fn files_written_with_print_hash_and_bput_read_back() {
    let mut writing = basic();
    writing.insert_tape(Tape::default());
    writing.set_tape_record(true);
    type_text(
        &mut writing,
        "C=OPENOUT\"F\":PRINT#C,1,-2.5,\"AB\":BPUT#C,65:CLOSE#C\n",
    )
    .expect("a key types each character");
    writing.set_key(Key::RETURN, true);
    wait(&mut writing, TAPE);
    writing.set_key(Key::RETURN, false);
    let tape = writing.eject_tape().expect("the tape written to");

    let reading = mode7_text(&with_tape(
        tape,
        "C=OPENIN\"F\":INPUT#C,A,B,C$:D=BGET#C:E=EOF#C:CLOSE#C:PRINT A=1 AND B=-2.5 AND C$=\"AB\" AND D=65 AND E",
    ));
    assert!(
        reading.iter().any(|line| line == "        -1"),
        "every value came back: {reading:?}"
    );
}
