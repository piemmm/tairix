//! The editor: LIST and LISTO, AUTO, DELETE, RENUMBER, line entry, and
//! what a statement that is no assignment reports.

use super::assert_screen;
use crate::rom::os::COPYRIGHT;

#[test]
fn list_takes_a_line_a_range_or_an_open_range() {
    assert_screen(
        &[
            "10 FOR I=1 TO 2",
            "20 REPEAT",
            "30 PRINT I",
            "40 UNTIL TRUE",
            "50 NEXT",
            "LIST 20,40",
            "LIST ,20",
            "LIST 40,",
            "LIST 30",
        ],
        50,
        &[
            ">10 FOR I=1 TO 2",
            ">20 REPEAT",
            ">30 PRINT I",
            ">40 UNTIL TRUE",
            ">50 NEXT",
            ">LIST 20,40",
            "   20 REPEAT",
            "   30 PRINT I",
            "   40 UNTIL TRUE",
            ">LIST ,20",
            "   10 FOR I=1 TO 2",
            "   20 REPEAT",
            ">LIST 40,",
            "   40 UNTIL TRUE",
            "   50 NEXT",
            ">LIST 30",
            "   30 PRINT I",
            ">",
        ],
    );
}

#[test]
fn listo_indents_loops_and_spaces_the_number() {
    assert_screen(
        &[
            "10 FOR I=1 TO 2",
            "20 REPEAT",
            "30 PRINT I:FOR J=1 TO 2:NEXT",
            "40 UNTIL TRUE",
            "50 NEXT:NEXT",
            "60PRINT\"FOR\"",
            "LISTO 7",
            "LIST",
            "LISTO 2",
            "LIST ,30",
        ],
        50,
        &[
            ">10 FOR I=1 TO 2",
            ">20 REPEAT",
            ">30 PRINT I:FOR J=1 TO 2:NEXT",
            ">40 UNTIL TRUE",
            ">50 NEXT:NEXT",
            ">60PRINT\"FOR\"",
            ">LISTO 7",
            ">LIST",
            "   10  FOR I=1 TO 2",
            "   20    REPEAT",
            "   30      PRINT I:FOR J=1 TO 2:NEXT",
            "   40      UNTIL TRUE",
            "   50    NEXT:NEXT",
            "   60 PRINT\"FOR\"",
            ">LISTO 2",
            ">LIST ,30",
            "   10 FOR I=1 TO 2",
            "   20   REPEAT",
            "   30   PRINT I:FOR J=1 TO 2:NEXT",
            ">",
        ],
    );
}

#[test]
fn listo_counts_loops_from_the_listing_and_never_below_none() {
    assert_screen(
        &[
            "10 NEXT",
            "20 FOR I=1 TO 2",
            "30 PRINT",
            "40 UNTIL 1",
            "50 REPEAT",
            "60 PRINT",
            "70 FOR J=1 TO 2:FOR K=1 TO 2",
            "80 PRINT",
            "LISTO 6",
            "LIST",
            "LISTO 7",
            "LIST 60",
        ],
        50,
        &[
            ">10 NEXT",
            ">20 FOR I=1 TO 2",
            ">30 PRINT",
            ">40 UNTIL 1",
            ">50 REPEAT",
            ">60 PRINT",
            ">70 FOR J=1 TO 2:FOR K=1 TO 2",
            ">80 PRINT",
            ">LISTO 6",
            ">LIST",
            "   10 NEXT",
            "   20 FOR I=1 TO 2",
            "   30   PRINT",
            "   40   UNTIL 1",
            "   50   REPEAT",
            "   60     PRINT",
            "   70     FOR J=1 TO 2:FOR K=1 TO 2",
            "   80         PRINT",
            ">LISTO 7",
            ">LIST 60",
            "   60  PRINT",
            ">",
        ],
    );
}

#[test]
fn listo_takes_an_expression_and_ends_the_line() {
    assert_screen(
        &[
            "LISTO 1:PRINT 5",
            "LISTO 2+1",
            "10 FOR I=1 TO 2",
            "LIST",
            "LIST 10 20",
            "LISTO 0",
        ],
        50,
        &[
            ">LISTO 1:PRINT 5",
            ">LISTO 2+1",
            ">10 FOR I=1 TO 2",
            ">LIST",
            "   10  FOR I=1 TO 2",
            ">LIST 10 20",
            "",
            "Syntax error",
            ">LISTO 0",
            ">",
        ],
    );
}

#[test]
fn delete_takes_two_lines_and_deletes_the_first_line_found() {
    assert_screen(
        &[
            "10 A=1",
            "20 B=2",
            "30 C=3",
            "40 D=4",
            "50 E=5",
            "DELETE 20,30",
            "DELETE 40",
            "DELETE 50,10",
            "LIST",
        ],
        50,
        &[
            ">10 A=1",
            ">20 B=2",
            ">30 C=3",
            ">40 D=4",
            ">50 E=5",
            ">DELETE 20,30",
            ">DELETE 40",
            "",
            "Syntax error",
            ">DELETE 50,10",
            ">LIST",
            "   10 A=1",
            "   40 D=4",
            ">",
        ],
    );
}

#[test]
fn renumber_follows_references_and_reports_missing_ones() {
    assert_screen(
        &[
            "10 GOTO 30",
            "20 GOSUB 40:ON A GOTO 10,30,99",
            "30 RESTORE 10:IF A THEN 20 ELSE 40",
            "40 RETURN",
            "RENUMBER 100,5",
            "LIST",
        ],
        60,
        &[
            ">10 GOTO 30",
            ">20 GOSUB 40:ON A GOTO 10,30,99",
            ">30 RESTORE 10:IF A THEN 20 ELSE 40",
            ">40 RETURN",
            ">RENUMBER 100,5",
            "Failed at 105",
            ">LIST",
            "  100 GOTO 110",
            "  105 GOSUB 115:ON A GOTO 100,110,99",
            "  110 RESTORE 100:IF A THEN 105 ELSE 115",
            "",
            "  115 RETURN",
            ">",
        ],
    );
}

#[test]
fn renumber_refuses_numbers_past_32767_and_takes_defaults() {
    assert_screen(
        &[
            "10 GOTO 20",
            "20 PRINT",
            "30 GOTO 10",
            "RENUMBER 32000,1000",
            "RENUMBER 10,0",
            "RENUMBER 1000",
            "LIST",
            "RENUMBER ,2",
            "LIST",
        ],
        60,
        &[
            ">10 GOTO 20",
            ">20 PRINT",
            ">30 GOTO 10",
            ">RENUMBER 32000,1000",
            "",
            "Silly",
            ">RENUMBER 10,0",
            "",
            "Silly",
            ">RENUMBER 1000",
            ">LIST",
            " 1000 GOTO 1010",
            " 1010 PRINT",
            " 1020 GOTO 1000",
            ">RENUMBER ,2",
            ">LIST",
            "   10 GOTO 12",
            "   12 PRINT",
            "   14 GOTO 10",
            ">",
        ],
    );
}

#[test]
fn odd_line_numbers() {
    assert_screen(
        &[
            "99999",
            "65536",
            "65535",
            "0 PRINT \"Z\"",
            "10 20",
            "RUN",
            "  25  PRINT 2",
            "LIST",
        ],
        50,
        &[
            ">99999",
            "",
            "Syntax error",
            ">65536",
            "",
            "Syntax error",
            ">65535",
            "",
            "Syntax error",
            ">0 PRINT \"Z\"",
            ">10 20",
            ">RUN",
            "Z",
            "",
            "Syntax error at line 10",
            ">  25  PRINT 2",
            ">LIST",
            "    0 PRINT \"Z\"",
            "   10 20",
            "   25  PRINT 2",
            ">",
        ],
    );
}

#[test]
fn statements_that_cannot_be_assignments_are_syntax_errors() {
    assert_screen(
        &[")", "+", "(", "@", ".", "Q+"],
        40,
        &[
            ">)",
            "",
            "Syntax error",
            ">+",
            "",
            "Syntax error",
            ">(",
            "",
            "Syntax error",
            ">@",
            "",
            "Syntax error",
            ">.",
            "",
            "Syntax error",
            ">Q+",
            "",
            "Mistake",
            ">",
        ],
    );
}

#[test]
fn report_counts_its_characters() {
    assert_screen(
        &["REPORT:PRINT COUNT", "Q", "REPORT:PRINT COUNT"],
        40,
        &[
            ">REPORT:PRINT COUNT",
            "",
            COPYRIGHT,
            "         0",
            ">Q",
            "",
            "Mistake",
            ">REPORT:PRINT COUNT",
            "",
            "Mistake         7",
            ">",
        ],
    );
}

#[test]
fn a_damaged_program_is_reported_after_an_error_end_and_list() {
    // The second record's CR overwritten: each error, END and LIST then
    // finds TOP again and says so, ERR keeping the error's number.
    assert_screen(
        &[
            "20 PRINT \"B\"",
            "30 PRINT \"C\"",
            "?(PAGE+?(PAGE+3))=0",
            "PRINT X",
            "PRINT ERR",
            "LIST",
            "PRINT ~TOP-PAGE",
            "END",
        ],
        60,
        &[
            ">20 PRINT \"B\"",
            ">30 PRINT \"C\"",
            ">?(PAGE+?(PAGE+3))=0",
            ">PRINT X",
            "",
            "No such variable",
            "",
            "Bad program",
            ">PRINT ERR",
            "        26",
            ">LIST",
            "",
            "Bad program",
            ">PRINT ~TOP-PAGE",
            "         A",
            ">END",
            "",
            "Bad program",
            ">",
        ],
    );
}

#[test]
fn a_record_too_short_steps_on_before_it_is_found_bad() {
    // A length of one steps TOP a byte on, onto the line number's high
    // byte, where BASIC II finds no CR.
    assert_screen(
        &["20 PRINT \"B\"", "?(PAGE+3)=1", "LIST", "PRINT TOP-PAGE"],
        40,
        &[
            ">20 PRINT \"B\"",
            ">?(PAGE+3)=1",
            ">LIST",
            "",
            "Bad program",
            ">PRINT TOP-PAGE",
            "         1",
            ">",
        ],
    );
}

#[test]
fn run_finds_no_first_line_without_its_cr() {
    assert_screen(
        &["20 PRINT \"B\"", "?PAGE=0", "RUN"],
        40,
        &[
            ">20 PRINT \"B\"",
            ">?PAGE=0",
            ">RUN",
            "",
            "Syntax error",
            "",
            "Bad program",
            ">",
        ],
    );
}

#[test]
fn commands_are_only_a_typed_line_s_first_keyword() {
    assert_screen(
        &[
            "10 PRINT 1",
            "20 LIST",
            "PRINT:LIST",
            "PRINT:OLD",
            "RUN",
            "LIST",
        ],
        60,
        &[
            ">10 PRINT 1",
            ">20 LIST",
            ">PRINT:LIST",
            "",
            "",
            "Syntax error",
            ">PRINT:OLD",
            "",
            "",
            "Syntax error",
            ">RUN",
            "         1",
            "",
            "Syntax error at line 20",
            ">LIST",
            "   10 PRINT 1",
            "   20 LIST",
            ">",
        ],
    );
}

/// The regression: entering at the execution address SAVE gives a program
/// kept the program and its settings, where BASIC II starts afresh there,
/// as the language entry does.
#[test]
fn a_saved_program_s_execution_address_starts_basic_afresh() {
    assert_screen(
        &[
            "10 PRINT \"HI\"",
            "LISTO 7",
            "@%=&2020A",
            "CALL&8023",
            "LIST",
            "P.~@%",
            "OLD",
            "LIST",
        ],
        50,
        &[
            ">10 PRINT \"HI\"",
            ">LISTO 7",
            ">@%=&2020A",
            ">CALL&8023",
            ">LIST",
            ">P.~@%",
            "       90A",
            ">OLD",
            ">LIST",
            "   10 PRINT \"HI\"",
            ">",
        ],
    );
}
