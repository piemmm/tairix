//! Programs typed at BASIC's prompt, each screen against the one the
//! original leaves, recorded from it run as an opaque program: `|` is
//! RETURN.

use alloc::string::String;
use alloc::vec::Vec;

use crate::machine::c64::keyboard::Key;
use crate::machine::c64::Standard;
use crate::rom::c64::system;
use crate::rom::c64::testing::{machine, press, run_for, screen_rows, type_text, JIFFY};

/// The screen after CLR and `keys` typed at READY, then `jiffies` let pass.
fn screen_after(keys: &str, jiffies: u64) -> Vec<String> {
    let system = system().expect("the BASIC and KERNAL ROMs assemble");
    let mut machine = machine(Standard::Pal, system.basic(), system.kernal());
    run_for(&mut machine, 4 * Standard::Pal.clock());
    press(&mut machine, &[Key::LEFT_SHIFT], Key::HOME);
    type_text(&mut machine, &keys.replace('|', "\n"));
    run_for(&mut machine, jiffies * JIFFY);
    screen_rows(&machine)
}

/// The screen after CLR, `keys` and the default pause.
fn screen(keys: &str) -> Vec<String> {
    screen_after(keys, 90)
}

#[test]
fn printing_numbers_and_text() {
    assert_eq!(
        screen(
            "PRINT 1+1|PRINT \"HELLO\";|PRINT 1/3|PRINT -1.5E-10,1E20|PRINT .1;.01;.001;1E9;123456789|"
        ),
        [
            "PRINT 1+1",
            " 2",
            "",
            "READY.",
            "PRINT \"HELLO\";",
            "HELLO",
            "READY.",
            "PRINT 1/3",
            " .333333333",
            "",
            "READY.",
            "PRINT -1.5E-10,1E20",
            "-1.5E-10   1E+20",
            "",
            "READY.",
            "PRINT .1;.01;.001;1E9;123456789",
            " .1  .01  1E-03  1E+09  123456789",
            "",
            "READY.",
        ]
    );
}

#[test]
fn arithmetic_and_number_functions() {
    assert_eq!(
        screen(
            "PRINT 2^.5,SQR(2),INT(-3.5),ABS(-4)|PRINT SGN(-2);SGN(0);SGN(3)|PRINT 10/4;7*6;100-0.1|"
        ),
        [
            "PRINT 2^.5,SQR(2),INT(-3.5),ABS(-4)",
            " 1.41421356          1.41421356",
            "-4         4",
            "",
            "READY.",
            "PRINT SGN(-2);SGN(0);SGN(3)",
            "-1  0  1",
            "",
            "READY.",
            "PRINT 10/4;7*6;100-0.1",
            " 2.5  42  99.9",
            "",
            "READY.",
        ]
    );
}

#[test]
fn string_functions() {
    assert_eq!(
        screen(
            "A$=\"HELLO WORLD\"|PRINT LEFT$(A$,5);\"/\";RIGHT$(A$,5);\"/\";MID$(A$,4,3)|PRINT LEN(A$);ASC(A$);CHR$(65);VAL(\"12.5E1\");STR$(-7)|"
        ),
        [
            "A$=\"HELLO WORLD\"",
            "",
            "READY.",
            "PRINT LEFT$(A$,5);\"/\";RIGHT$(A$,5);\"/\";M",
            "ID$(A$,4,3)",
            "HELLO/WORLD/LO",
            "",
            "READY.",
            "PRINT LEN(A$);ASC(A$);CHR$(65);VAL(\"12.5",
            "E1\");STR$(-7)",
            " 11  72 A 125 -7",
            "",
            "READY.",
        ]
    );
}

#[test]
fn for_next_runs_its_body() {
    assert_eq!(
        screen("10 FOR I=1 TO 3|20 PRINT I;|30 NEXT|40 PRINT \"DONE\"|RUN|"),
        [
            "10 FOR I=1 TO 3",
            "20 PRINT I;",
            "30 NEXT",
            "40 PRINT \"DONE\"",
            "RUN",
            " 1  2  3 DONE",
            "",
            "READY.",
        ]
    );
}

#[test]
fn gosub_returns() {
    assert_eq!(
        screen("10 GOSUB 100:PRINT \"BACK\":END|100 PRINT \"SUB\":RETURN|RUN|"),
        [
            "10 GOSUB 100:PRINT \"BACK\":END",
            "100 PRINT \"SUB\":RETURN",
            "RUN",
            "SUB",
            "BACK",
            "",
            "READY.",
        ]
    );
}

#[test]
fn on_goto_picks_its_target() {
    assert_eq!(
        screen(
            "10 FOR I=1TO3:ON I GOTO 20,30,40|20 PRINT\"A\";:NEXT|30 PRINT\"B\";:NEXT|40 PRINT\"C\":END|RUN|"
        ),
        [
            "10 FOR I=1TO3:ON I GOTO 20,30,40",
            "20 PRINT\"A\";:NEXT",
            "30 PRINT\"B\";:NEXT",
            "40 PRINT\"C\":END",
            "RUN",
            "ABC",
            "",
            "READY.",
        ]
    );
}

#[test]
fn if_then_branches() {
    assert_eq!(
        screen(
            "10 A=5:IF A>3 THEN PRINT\"BIG\"|20 IF A<3 THEN PRINT\"SMALL\"|30 IF A THEN 50|40 PRINT\"NO\"|50 PRINT\"YES\"|RUN|"
        ),
        [
            "10 A=5:IF A>3 THEN PRINT\"BIG\"",
            "20 IF A<3 THEN PRINT\"SMALL\"",
            "30 IF A THEN 50",
            "40 PRINT\"NO\"",
            "50 PRINT\"YES\"",
            "RUN",
            "BIG",
            "YES",
            "",
            "READY.",
        ]
    );
}

#[test]
fn arrays_and_their_bounds() {
    assert_eq!(
        screen("DIM A(3),B$(2):A(3)=7:B$(1)=\"X\":PRINT A(3);B$(1);A(0)|PRINT A(4)|"),
        [
            "DIM A(3),B$(2):A(3)=7:B$(1)=\"X\":PRINT A(",
            "3);B$(1);A(0)",
            " 7 X 0",
            "",
            "READY.",
            "PRINT A(4)",
            "",
            "?BAD SUBSCRIPT  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn def_fn_defines_a_function() {
    assert_eq!(
        screen("10 DEF FNA(X)=X*X+1|20 PRINT FNA(3);FNA(-2)|RUN|"),
        [
            "10 DEF FNA(X)=X*X+1",
            "20 PRINT FNA(3);FNA(-2)",
            "RUN",
            " 10  5",
            "",
            "READY.",
        ]
    );
}

#[test]
fn read_data_and_restore() {
    assert_eq!(
        screen("10 READ A,B$,C|20 PRINT A;B$;C|30 DATA 1,\"TWO\",3|40 RESTORE:READ A:PRINT A|RUN|"),
        [
            "10 READ A,B$,C",
            "20 PRINT A;B$;C",
            "30 DATA 1,\"TWO\",3",
            "40 RESTORE:READ A:PRINT A",
            "RUN",
            " 1 TWO 3",
            " 1",
            "",
            "READY.",
        ]
    );
}

#[test]
fn division_redim_type_and_line_errors() {
    assert_eq!(
        screen("PRINT 1/0|DIM A(5):DIM A(5)|PRINT \"A\"+1|GOTO 100|"),
        [
            "PRINT 1/0",
            "",
            "?DIVISION BY ZERO  ERROR",
            "READY.",
            "DIM A(5):DIM A(5)",
            "",
            "?REDIM'D ARRAY  ERROR",
            "READY.",
            "PRINT \"A\"+1",
            "",
            "?TYPE MISMATCH  ERROR",
            "READY.",
            "GOTO 100",
            "",
            "?UNDEF'D STATEMENT  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn direct_cont_function_and_quantity_errors() {
    assert_eq!(
        screen("INPUT A|CONT|PRINT FNA(1)|A$=LEFT$(\"A\",300)|"),
        [
            "INPUT A",
            "",
            "?ILLEGAL DIRECT  ERROR",
            "READY.",
            "CONT",
            "",
            "?CAN'T CONTINUE  ERROR",
            "READY.",
            "PRINT FNA(1)",
            "",
            "?UNDEF'D FUNCTION  ERROR",
            "READY.",
            "A$=LEFT$(\"A\",300)",
            "",
            "?ILLEGAL QUANTITY  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn data_return_next_and_syntax_errors() {
    assert_eq!(
        screen("READ A|RETURN|NEXT|PRINT )|"),
        [
            "READ A",
            "",
            "?OUT OF DATA  ERROR",
            "READY.",
            "RETURN",
            "",
            "?RETURN WITHOUT GOSUB  ERROR",
            "READY.",
            "NEXT",
            "",
            "?NEXT WITHOUT FOR  ERROR",
            "READY.",
            "PRINT )",
            "",
            "?SYNTAX  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn file_overflow_and_length_errors() {
    assert_eq!(
        screen("PRINT#1|OPEN 1,3:OPEN 1,3|X=1E39|A$=\"X\":FORI=1TO8:A$=A$+A$:NEXT|"),
        [
            "PRINT#1",
            "",
            "?FILE NOT OPEN  ERROR",
            "READY.",
            "OPEN 1,3:OPEN 1,3",
            "",
            "?FILE OPEN  ERROR",
            "READY.",
            "X=1E39",
            "",
            "?OVERFLOW  ERROR",
            "READY.",
            "A$=\"X\":FORI=1TO8:A$=A$+A$:NEXT",
            "",
            "?STRING TOO LONG  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn device_and_formula_errors() {
    assert_eq!(
        screen("LOAD\"X\",8|A$=\"\"+(\"\"+(\"\"+(\"\"+\"\")))|"),
        [
            "LOAD\"X\",8",
            "",
            "SEARCHING FOR X",
            "?DEVICE NOT PRESENT  ERROR",
            "READY.",
            "A$=\"\"+(\"\"+(\"\"+(\"\"+\"\")))",
            "",
            "?FORMULA TOO COMPLEX  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn tab_spc_and_print_zones() {
    assert_eq!(
        screen("PRINT \"A\";TAB(5);\"B\";SPC(3);\"C\"|PRINT 1,2,3|PRINT \"X\";POS(0)|"),
        [
            "PRINT \"A\";TAB(5);\"B\";SPC(3);\"C\"",
            "A    B   C",
            "",
            "READY.",
            "PRINT 1,2,3",
            " 1         2         3",
            "",
            "READY.",
            "PRINT \"X\";POS(0)",
            "X 1",
            "",
            "READY.",
        ]
    );
}

#[test]
fn fre_and_ti_string() {
    assert_eq!(
        screen("PRINT FRE(0)|TI$=\"123456\":PRINT TI$|"),
        [
            "PRINT FRE(0)",
            "-26627",
            "",
            "READY.",
            "TI$=\"123456\":PRINT TI$",
            "123456",
            "",
            "READY.",
        ]
    );
}

#[test]
fn list_spells_out_keywords() {
    assert_eq!(
        screen("10 PRINT \"HI\":REM TEST|20 IF X THEN GOTO 10|LIST|"),
        [
            "10 PRINT \"HI\":REM TEST",
            "20 IF X THEN GOTO 10",
            "LIST",
            "",
            "10 PRINT \"HI\":REM TEST",
            "20 IF X THEN GOTO 10",
            "READY.",
        ]
    );
}

#[test]
fn for_steps_down_and_runs_at_least_once() {
    assert_eq!(
        screen("FOR I=10 TO 1 STEP -3:PRINT I;:NEXT|FOR I=1 TO 0:PRINT\"X\":NEXT:PRINT\"Y\"|"),
        [
            "FOR I=10 TO 1 STEP -3:PRINT I;:NEXT",
            " 10  7  4  1",
            "READY.",
            "FOR I=1 TO 0:PRINT\"X\":NEXT:PRINT\"Y\"",
            "X",
            "Y",
            "",
            "READY.",
        ]
    );
}

#[test]
fn collected_strings_leave_the_free_memory_the_original_leaves() {
    assert_eq!(
        screen_after(
            "10 FOR I=1 TO 300:A$=STR$(I):B$=A$+\"X\":NEXT|20 PRINT A$;B$;FRE(0)|RUN|",
            1500
        ),
        [
            "10 FOR I=1 TO 300:A$=STR$(I):B$=A$+\"X\":N",
            "EXT",
            "20 PRINT A$;B$;FRE(0)",
            "RUN",
            " 300 300X-26710",
            "",
            "READY.",
        ]
    );
}

#[test]
fn integer_variables() {
    assert_eq!(
        screen("A%=-3.7:B%=32767:PRINT A%;B%|C%=40000|"),
        [
            "A%=-3.7:B%=32767:PRINT A%;B%",
            "-4  32767",
            "",
            "READY.",
            "C%=40000",
            "",
            "?ILLEGAL QUANTITY  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn rnd_seeds_and_steps() {
    assert_eq!(
        screen("PRINT RND(-1);RND(1);RND(1)|"),
        [
            "PRINT RND(-1);RND(1);RND(1)",
            " 2.99196472E-08  .328780872  .978964086",
            "",
            "",
            "READY.",
        ]
    );
}

#[test]
fn logical_and_relational_operators() {
    assert_eq!(
        screen("PRINT 5 AND 3;5 OR 3;NOT 0;-(1<2);\"A\"<\"B\";2=2|"),
        [
            "PRINT 5 AND 3;5 OR 3;NOT 0;-(1<2);\"A\"<\"B",
            "\";2=2",
            " 1  7 -1  1 -1 -1",
            "",
            "READY.",
        ]
    );
}

#[test]
fn get_with_no_key_down() {
    assert_eq!(
        screen("10 GET A$:IF A$=\"\"THEN PRINT\"EMPTY\"|RUN|"),
        [
            "10 GET A$:IF A$=\"\"THEN PRINT\"EMPTY\"",
            "RUN",
            "EMPTY",
            "",
            "READY.",
        ]
    );
}

#[test]
fn input_with_a_prompt() {
    assert_eq!(
        screen("10 INPUT \"NAME\";N$:PRINT \"HI \";N$|RUN|BOB|"),
        [
            "10 INPUT \"NAME\";N$:PRINT \"HI \";N$",
            "RUN",
            "NAME? BOB",
            "HI BOB",
            "",
            "READY.",
        ]
    );
}

#[test]
fn input_asks_again_for_a_number() {
    assert_eq!(
        screen("10 INPUT A:PRINT A*2|RUN|X|5|"),
        [
            "10 INPUT A:PRINT A*2",
            "RUN",
            "? X",
            "?REDO FROM START",
            "? 5",
            " 10",
            "",
            "READY.",
        ]
    );
}

#[test]
fn input_ignores_what_is_extra() {
    assert_eq!(
        screen("10 INPUT A,B:PRINT A+B|RUN|1,2,3|"),
        [
            "10 INPUT A,B:PRINT A+B",
            "RUN",
            "? 1,2,3",
            "?EXTRA IGNORED",
            " 3",
            "",
            "READY.",
        ]
    );
}

#[test]
fn transcendental_functions() {
    assert_eq!(
        screen("PRINT SIN(1);COS(1);TAN(1);ATN(1)|PRINT LOG(10);EXP(1)|"),
        [
            "PRINT SIN(1);COS(1);TAN(1);ATN(1)",
            " .841470985  .540302306  1.55740772  .78",
            "5398163",
            "",
            "READY.",
            "PRINT LOG(10);EXP(1)",
            " 2.30258509  2.71828183",
            "",
            "READY.",
        ]
    );
}

#[test]
fn stop_and_cont() {
    assert_eq!(
        screen("10 PRINT\"A\":STOP|20 PRINT\"B\"|RUN|CONT|"),
        [
            "10 PRINT\"A\":STOP",
            "20 PRINT\"B\"",
            "RUN",
            "A",
            "",
            "BREAK IN 10",
            "READY.",
            "CONT",
            "B",
            "",
            "READY.",
        ]
    );
}

#[test]
fn end_stops_quietly() {
    assert_eq!(
        screen("10 PRINT\"A\":END:PRINT\"B\"|RUN|"),
        ["10 PRINT\"A\":END:PRINT\"B\"", "RUN", "A", "", "READY.",]
    );
}

#[test]
fn peek_poke_sys_and_usr() {
    assert_eq!(
        screen("POKE 828,123:PRINT PEEK(828)|POKE 828,96:SYS 828:PRINT \"OK\"|PRINT USR(1)|"),
        [
            "POKE 828,123:PRINT PEEK(828)",
            " 123",
            "",
            "READY.",
            "POKE 828,96:SYS 828:PRINT \"OK\"",
            "OK",
            "",
            "READY.",
            "PRINT USR(1)",
            "",
            "?ILLEGAL QUANTITY  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn clr_and_new() {
    assert_eq!(
        screen("A=5:CLR:PRINT A|10 PRINT 1|NEW|LIST|"),
        [
            "A=5:CLR:PRINT A",
            " 0",
            "",
            "READY.",
            "10 PRINT 1",
            "NEW",
            "",
            "READY.",
            "LIST",
            "",
            "READY.",
        ]
    );
}

#[test]
fn numbers_in_and_out_of_scientific_notation() {
    assert_eq!(
        screen("PRINT 1E-5;1.5E-1;123.456;-0.5;1/7*7|PRINT 1E-3;.012;99999999.9;999999999;1E10|"),
        [
            "PRINT 1E-5;1.5E-1;123.456;-0.5;1/7*7",
            " 1E-05  .15  123.456 -.5  1",
            "",
            "READY.",
            "PRINT 1E-3;.012;99999999.9;999999999;1E1",
            "0",
            " 1E-03  .012  99999999.9  999999999  1E+",
            "10",
            "",
            "READY.",
        ]
    );
}

#[test]
fn string_comparisons() {
    assert_eq!(
        screen("PRINT \"AB\"=\"AB\";\"AB\"<\"ABC\";\"B\">\"A\"|"),
        [
            "PRINT \"AB\"=\"AB\";\"AB\"<\"ABC\";\"B\">\"A\"",
            "-1 -1 -1",
            "",
            "READY.",
        ]
    );
}

#[test]
fn val_reads_a_number() {
    assert_eq!(
        screen("PRINT VAL(\"1E\");VAL(\" 12 3\");VAL(\"-.5\");VAL(\"\")|"),
        [
            "PRINT VAL(\"1E\");VAL(\" 12 3\");VAL(\"-.5\");",
            "VAL(\"\")",
            " 1  123 -.5  0",
            "",
            "READY.",
        ]
    );
}

#[test]
fn substring_bounds_and_arguments() {
    assert_eq!(
        screen(
            "PRINT LEFT$(\"ABC\",1,2)|PRINT RIGHT$(\"ABC\",1,2)|PRINT MID$(\"ABC\",2);\"/\";MID$(\"ABC\",4);\"/\";LEFT$(\"ABC\",0);\"/\";RIGHT$(\"ABC\",5)|PRINT MID$(\"ABC\",0)|"
        ),
        [
            "PRINT LEFT$(\"ABC\",1,2)",
            "",
            "?SYNTAX  ERROR",
            "READY.",
            "PRINT RIGHT$(\"ABC\",1,2)",
            "",
            "?SYNTAX  ERROR",
            "READY.",
            "PRINT MID$(\"ABC\",2);\"/\";MID$(\"ABC\",4);\"/",
            "\";LEFT$(\"ABC\",0);\"/\";RIGHT$(\"ABC\",5)",
            "BC///ABC",
            "",
            "READY.",
            "PRINT MID$(\"ABC\",0)",
            "",
            "?ILLEGAL QUANTITY  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn get_is_refused_at_the_keyboard() {
    assert_eq!(
        screen("GET A$|INPUT#1,A|GET#1,A$|"),
        [
            "GET A$",
            "",
            "?ILLEGAL DIRECT  ERROR",
            "READY.",
            "INPUT#1,A",
            "",
            "?FILE NOT OPEN  ERROR",
            "READY.",
            "GET#1,A$",
            "",
            "?ILLEGAL DIRECT  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn input_from_an_open_file_is_refused_at_the_keyboard_after_its_comma() {
    assert_eq!(
        screen("OPEN1,0|INPUT#1|INPUT#1,A|PRINT\"AFTER\"|"),
        [
            "OPEN1,0",
            "",
            "READY.",
            "INPUT#1",
            "",
            "?SYNTAX  ERROR",
            "READY.",
            "INPUT#1,A",
            "",
            "?ILLEGAL DIRECT  ERROR",
            "READY.",
            "PRINT\"AFTER\"",
            "AFTER",
            "",
            "READY.",
        ]
    );
}

#[test]
fn tab_past_the_line_and_behind_the_cursor() {
    assert_eq!(
        screen("PRINT TAB(45);\"X\"|PRINT SPC(3);TAB(1);\"Y\"|"),
        [
            "PRINT TAB(45);\"X\"",
            "",
            "     X",
            "",
            "READY.",
            "PRINT SPC(3);TAB(1);\"Y\"",
            "   Y",
            "",
            "READY.",
        ]
    );
}

#[test]
fn input_asks_again_after_text_past_a_quoted_reply() {
    assert_eq!(
        screen("10 INPUT A$:PRINT \"[\";A$;\"]\"|RUN|\"AB\"CD|"),
        [
            "10 INPUT A$:PRINT \"[\";A$;\"]\"",
            "RUN",
            "? \"AB\"CD",
            "?REDO FROM START",
            "?",
        ]
    );
}

#[test]
fn clr_leaves_nothing_to_continue() {
    assert_eq!(
        screen("10 STOP:PRINT\"B\"|RUN|CLR|CONT|"),
        [
            "10 STOP:PRINT\"B\"",
            "RUN",
            "",
            "BREAK IN 10",
            "READY.",
            "CLR",
            "",
            "READY.",
            "CONT",
            "",
            "?CAN'T CONTINUE  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn subscripts_out_of_range() {
    assert_eq!(
        screen("DIM A(2):PRINT A(32768)|PRINT A(-1)|"),
        [
            "DIM A(2):PRINT A(32768)",
            "",
            "?ILLEGAL QUANTITY  ERROR",
            "READY.",
            "PRINT A(-1)",
            "",
            "?ILLEGAL QUANTITY  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn if_compares_a_substring() {
    assert_eq!(
        screen("A$=\"X\":IF LEFT$(A$,1)=\"X\" THEN PRINT\"Y\"|"),
        [
            "A$=\"X\":IF LEFT$(A$,1)=\"X\" THEN PRINT\"Y\"",
            "Y",
            "",
            "READY.",
        ]
    );
}

#[test]
fn val_reads_signs_and_exponents() {
    assert_eq!(
        screen("PRINT VAL(\"1E+2\");VAL(\"+5\");VAL(\"-5\");VAL(\"1E-2\")|"),
        [
            "PRINT VAL(\"1E+2\");VAL(\"+5\");VAL(\"-5\");VA",
            "L(\"1E-2\")",
            " 100  5 -5  .01",
            "",
            "READY.",
        ]
    );
}

#[test]
fn numbers_rounded_to_nine_digits() {
    assert_eq!(
        screen("PRINT 100000000;1E8;123456789.7;.000123;-0;1/3*3|"),
        [
            "PRINT 100000000;1E8;123456789.7;.000123;",
            "-0;1/3*3",
            " 100000000  100000000  123456790  1.23E-",
            "04  0  1",
            "",
            "READY.",
        ]
    );
}

#[test]
fn operator_precedence() {
    assert_eq!(
        screen("PRINT -2^2;2^3^2;NOT 1+1;1+2*3-4/2|"),
        [
            "PRINT -2^2;2^3^2;NOT 1+1;1+2*3-4/2",
            "-4  64 -3  5",
            "",
            "READY.",
        ]
    );
}

#[test]
fn str_and_character_functions() {
    assert_eq!(
        screen("PRINT STR$(1E10);LEN(STR$(5));ASC(\"A\");CHR$(147)=\"\";STR$(0)|"),
        [
            "PRINT STR$(1E10);LEN(STR$(5));ASC(\"A\");C",
            "HR$(147)=\"\";STR$(0)",
            " 1E+10 2  65  0  0",
            "",
            "READY.",
        ]
    );
}

#[test]
fn functions_call_functions() {
    assert_eq!(
        screen("10 DEF FNA(X)=X+1:DEF FNB(X)=FNA(X)*2|20 PRINT FNB(3)|RUN|"),
        [
            "10 DEF FNA(X)=X+1:DEF FNB(X)=FNA(X)*2",
            "20 PRINT FNB(3)",
            "RUN",
            " 8",
            "",
            "READY.",
        ]
    );
}

#[test]
fn nested_gosubs_return() {
    assert_eq!(
        screen("10 GOSUB 20:PRINT\"OK\":END|20 GOSUB 30:RETURN|30 RETURN|RUN|"),
        [
            "10 GOSUB 20:PRINT\"OK\":END",
            "20 GOSUB 30:RETURN",
            "30 RETURN",
            "RUN",
            "OK",
            "",
            "READY.",
        ]
    );
}

#[test]
fn next_closes_two_loops() {
    assert_eq!(
        screen("10 FOR I=1 TO 2:FOR J=1 TO 2:PRINT I;J;:NEXT J,I|RUN|"),
        [
            "10 FOR I=1 TO 2:FOR J=1 TO 2:PRINT I;J;:",
            "NEXT J,I",
            "RUN",
            " 1  1  1  2  2  1  2  2",
            "READY.",
        ]
    );
}

#[test]
fn an_error_leaves_nothing_to_continue() {
    assert_eq!(
        screen("10 PRINT\"A\":PRINT 1/0:PRINT\"B\"|RUN|CONT|"),
        [
            "10 PRINT\"A\":PRINT 1/0:PRINT\"B\"",
            "RUN",
            "A",
            "",
            "?DIVISION BY ZERO  ERROR IN 10",
            "READY.",
            "CONT",
            "",
            "?CAN'T CONTINUE  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn cont_resumes_after_a_direct_command() {
    assert_eq!(
        screen("10 PRINT\"A\":STOP:PRINT\"B\"|RUN|PRINT 5|CONT|"),
        [
            "10 PRINT\"A\":STOP:PRINT\"B\"",
            "RUN",
            "A",
            "",
            "BREAK IN 10",
            "READY.",
            "PRINT 5",
            " 5",
            "",
            "READY.",
            "CONT",
            "B",
            "",
            "READY.",
        ]
    );
}

#[test]
fn cont_resumes_after_end() {
    assert_eq!(
        screen("10 PRINT\"A\":END:PRINT\"B\"|RUN|CONT|"),
        [
            "10 PRINT\"A\":END:PRINT\"B\"",
            "RUN",
            "A",
            "",
            "READY.",
            "CONT",
            "B",
            "",
            "READY.",
        ]
    );
}

#[test]
fn editing_a_line_leaves_nothing_to_continue() {
    assert_eq!(
        screen("10 PRINT\"A\":STOP:PRINT\"B\"|RUN|20 REM|CONT|"),
        [
            "10 PRINT\"A\":STOP:PRINT\"B\"",
            "RUN",
            "A",
            "",
            "BREAK IN 10",
            "READY.",
            "20 REM",
            "CONT",
            "",
            "?CAN'T CONTINUE  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn a_direct_error_leaves_nothing_to_continue() {
    assert_eq!(
        screen("10 PRINT\"A\":STOP:PRINT\"B\"|RUN|PRINT 1/0|CONT|"),
        [
            "10 PRINT\"A\":STOP:PRINT\"B\"",
            "RUN",
            "A",
            "",
            "BREAK IN 10",
            "READY.",
            "PRINT 1/0",
            "",
            "?DIVISION BY ZERO  ERROR",
            "READY.",
            "CONT",
            "",
            "?CAN'T CONTINUE  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn list_takes_a_range() {
    assert_eq!(
        screen("10 REM A|20 REM B|30 REM C|LIST 20|LIST -20|LIST 20-|"),
        [
            "10 REM A", "20 REM B", "30 REM C", "LIST 20", "", "20 REM B", "", "READY.",
            "LIST -20", "", "10 REM A", "20 REM B", "", "READY.", "LIST 20-", "", "20 REM B",
            "30 REM C", "READY.",
        ]
    );
}

#[test]
fn lines_replaced_and_deleted() {
    assert_eq!(
        screen("10 PRINT\"A\"|20 PRINT\"B\"|10 PRINT\"C\"|20|LIST|"),
        [
            "10 PRINT\"A\"",
            "20 PRINT\"B\"",
            "10 PRINT\"C\"",
            "20",
            "LIST",
            "",
            "10 PRINT\"C\"",
            "READY.",
        ]
    );
}

#[test]
fn strings_assigned_and_array_strings() {
    assert_eq!(
        screen("A$=\"AB\":B$=A$:A$=\"C\":PRINT B$;A$|A$(5)=\"X\":PRINT A$(5);LEN(A$(4))|"),
        [
            "A$=\"AB\":B$=A$:A$=\"C\":PRINT B$;A$",
            "ABC",
            "",
            "READY.",
            "A$(5)=\"X\":PRINT A$(5);LEN(A$(4))",
            "X 0",
            "",
            "READY.",
        ]
    );
}

#[test]
fn arrays_of_two_dimensions_and_integers() {
    assert_eq!(
        screen(
            "DIM A(2,3):A(2,3)=5:A(1,1)=2:PRINT A(2,3);A(1,1);A(0,0)|DIM B%(3):B%(1)=-5:PRINT B%(1)|"
        ),
        [
            "DIM A(2,3):A(2,3)=5:A(1,1)=2:PRINT A(2,3",
            ");A(1,1);A(0,0)",
            " 5  2  0",
            "",
            "READY.",
            "DIM B%(3):B%(1)=-5:PRINT B%(1)",
            "-5",
            "",
            "READY.",
        ]
    );
}

#[test]
fn on_gosub_returns_to_its_statement() {
    assert_eq!(
        screen(
            "10 FOR X=1 TO 2:ON X GOSUB 30,40:NEXT:END|30 PRINT\"ONE\":RETURN|40 PRINT\"TWO\":RETURN|RUN|"
        ),
        [
            "10 FOR X=1 TO 2:ON X GOSUB 30,40:NEXT:EN",
            "D",
            "30 PRINT\"ONE\":RETURN",
            "40 PRINT\"TWO\":RETURN",
            "RUN",
            "ONE",
            "TWO",
            "",
            "READY.",
        ]
    );
}

#[test]
fn data_quoted_unquoted_and_numbers() {
    assert_eq!(
        screen("10 READ A$,B,C$:PRINT A$;\"/\";B;\"/\";C$|20 DATA \" SP \",-3.5,UN QUOTED|RUN|"),
        [
            "10 READ A$,B,C$:PRINT A$;\"/\";B;\"/\";C$",
            "20 DATA \" SP \",-3.5,UN QUOTED",
            "RUN",
            " SP /-3.5 /UN QUOTED",
            "",
            "READY.",
        ]
    );
}

#[test]
fn read_of_a_word_into_a_number_reports_the_data_line() {
    assert_eq!(
        screen("10 READ A|20 DATA X|RUN|"),
        [
            "10 READ A",
            "20 DATA X",
            "RUN",
            "",
            "?SYNTAX  ERROR IN 20",
            "READY.",
        ]
    );
}

#[test]
fn arguments_outside_a_function_s_domain() {
    assert_eq!(
        screen("PRINT SQR(-1)|PRINT LOG(0)|PRINT ASC(\"\")|PRINT CHR$(256)|"),
        [
            "PRINT SQR(-1)",
            "",
            "?ILLEGAL QUANTITY  ERROR",
            "READY.",
            "PRINT LOG(0)",
            "",
            "?ILLEGAL QUANTITY  ERROR",
            "READY.",
            "PRINT ASC(\"\")",
            "",
            "?ILLEGAL QUANTITY  ERROR",
            "READY.",
            "PRINT CHR$(256)",
            "",
            "?ILLEGAL QUANTITY  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn overflow_and_powers() {
    assert_eq!(
        screen("PRINT 1E38*10|PRINT VAL(\"1E39\")|PRINT INT(1E10);0^0;(-8)^2|"),
        [
            "PRINT 1E38*10",
            "",
            "?OVERFLOW  ERROR",
            "READY.",
            "PRINT VAL(\"1E39\")",
            "",
            "?OVERFLOW  ERROR",
            "READY.",
            "PRINT INT(1E10);0^0;(-8)^2",
            " 1E+10  1  64",
            "",
            "READY.",
        ]
    );
}

#[test]
fn addresses_and_integers_at_their_limits() {
    assert_eq!(
        screen("PRINT PEEK(65535)>=0|POKE 65536,1|A%=-32768:PRINT A%|"),
        [
            "PRINT PEEK(65535)>=0",
            "-1",
            "",
            "READY.",
            "POKE 65536,1",
            "",
            "?ILLEGAL QUANTITY  ERROR",
            "READY.",
            "A%=-32768:PRINT A%",
            "-32768",
            "",
            "READY.",
        ]
    );
}

#[test]
fn a_point_alone_is_zero() {
    assert_eq!(
        screen("PRINT .;VAL(\".\");-.5E1;1E+1|"),
        [
            "PRINT .;VAL(\".\");-.5E1;1E+1",
            " 0  0 -5  10",
            "",
            "READY.",
        ]
    );
}

#[test]
fn endless_gosub_runs_out_of_memory() {
    assert_eq!(
        screen("10 GOSUB 10|RUN|"),
        [
            "10 GOSUB 10",
            "RUN",
            "",
            "?OUT OF MEMORY  ERROR IN 10",
            "READY.",
        ]
    );
}

#[test]
fn next_of_another_variable() {
    assert_eq!(
        screen("10 FOR I=1 TO 2:NEXT J|RUN|"),
        [
            "10 FOR I=1 TO 2:NEXT J",
            "RUN",
            "",
            "?NEXT WITHOUT FOR  ERROR IN 10",
            "READY.",
        ]
    );
}

#[test]
fn for_of_a_running_variable_replaces_its_loop() {
    assert_eq!(
        screen("10 FOR I=1 TO 2:FOR I=5 TO 6:PRINT I;:NEXT:PRINT\"/\";|RUN|"),
        [
            "10 FOR I=1 TO 2:FOR I=5 TO 6:PRINT I;:NE",
            "XT:PRINT\"/\";",
            "RUN",
            " 5  6 /",
            "READY.",
        ]
    );
}

#[test]
fn absent_devices() {
    assert_eq!(
        screen("OPEN 1,4:PRINT#1,\"X\"|CLOSE 1|SAVE\"X\",8|VERIFY\"X\",8|"),
        [
            "OPEN 1,4:PRINT#1,\"X\"",
            "",
            "?DEVICE NOT PRESENT  ERROR",
            "READY.",
            "CLOSE 1",
            "",
            "READY.",
            "SAVE\"X\",8",
            "",
            "?DEVICE NOT PRESENT  ERROR",
            "READY.",
            "VERIFY\"X\",8",
            "",
            "SEARCHING FOR X",
            "?DEVICE NOT PRESENT  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn st_reads_the_status() {
    assert_eq!(screen("PRINT ST|"), ["PRINT ST", " 0", "", "READY.",]);
}

#[test]
fn def_fn_of_a_string_argument() {
    assert_eq!(
        screen("10 DEF FNA(X$)=1|RUN|"),
        [
            "10 DEF FNA(X$)=1",
            "RUN",
            "",
            "?TYPE MISMATCH  ERROR IN 10",
            "READY.",
        ]
    );
}

#[test]
fn a_string_of_255_characters() {
    assert_eq!(
        screen_after("A$=\"\":FORI=1TO255:A$=A$+\"X\":NEXT:PRINT LEN(A$)|", 900),
        [
            "A$=\"\":FORI=1TO255:A$=A$+\"X\":NEXT:PRINT L",
            "EN(A$)",
            " 255",
            "",
            "READY.",
        ]
    );
}

#[test]
fn collection_with_string_arrays() {
    assert_eq!(
        screen_after(
            "10 DIM S$(50):FOR J=1 TO 3:FOR I=0 TO 50:S$(I)=STR$(I*J):NEXT:NEXT|20 PRINT S$(50);FRE(0)|RUN|",
            1500
        ),
        [
            "10 DIM S$(50):FOR J=1 TO 3:FOR I=0 TO 50",
            ":S$(I)=STR$(I*J):NEXT:NEXT",
            "20 PRINT S$(50);FRE(0)",
            "RUN",
            " 150-27036",
            "",
            "READY.",
        ]
    );
}

#[test]
fn page_three_vectors_and_usr_hold_their_defaults() {
    assert_eq!(
        screen("FOR I=768 TO 779:PRINT PEEK(I);:NEXT|PRINT PEEK(3);PEEK(4);PEEK(5);PEEK(6);PEEK(784);PEEK(785);PEEK(786)|"),
        [
            "FOR I=768 TO 779:PRINT PEEK(I);:NEXT",
            " 139  227  131  164  124  165  26  167",
            "228  167  134  174",
            "READY.",
            "PRINT PEEK(3);PEEK(4);PEEK(5);PEEK(6);PE",
            "EK(784);PEEK(785);PEEK(786)",
            " 170  177  145  179  76  72  178",
            "",
            "READY.",
        ]
    );
}

#[test]
fn a_usr_routine_converts_through_the_fixed_addresses() {
    assert_eq!(
        screen(
            "10 FOR I=0 TO 6:READ B:POKE 828+I,B:NEXT|20 DATA 32,170,177,200,76,145,179|30 POKE 785,60:POKE 786,3|40 PRINT USR(41);USR(-5)|RUN|"
        ),
        [
            "10 FOR I=0 TO 6:READ B:POKE 828+I,B:NEXT",
            "",
            "20 DATA 32,170,177,200,76,145,179",
            "30 POKE 785,60:POKE 786,3",
            "40 PRINT USR(41);USR(-5)",
            "RUN",
            " 42 -4",
            "",
            "READY.",
        ]
    );
}

#[test]
fn let_assigns_as_a_bare_assignment_does() {
    assert_eq!(
        screen("LET A=5:LET B$=\"X\":PRINT A;B$|"),
        ["LET A=5:LET B$=\"X\":PRINT A;B$", " 5 X", "", "READY."]
    );
}

#[test]
fn wait_returns_once_its_bits_show() {
    assert_eq!(
        screen("POKE 828,1:WAIT 828,1:PRINT \"ONE\"|POKE 828,0:WAIT 828,1,1:PRINT \"TWO\"|"),
        [
            "POKE 828,1:WAIT 828,1:PRINT \"ONE\"",
            "ONE",
            "",
            "READY.",
            "POKE 828,0:WAIT 828,1,1:PRINT \"TWO\"",
            "TWO",
            "",
            "READY.",
        ]
    );
}

#[test]
fn wait_with_no_bits_to_show_never_returns() {
    assert_eq!(
        screen("WAIT 828,0:PRINT \"THREE\"|"),
        ["WAIT 828,0:PRINT \"THREE\""]
    );
}

#[test]
fn cmd_sends_print_and_list_to_a_file() {
    assert_eq!(
        screen("OPEN 1,3:CMD 1:PRINT \"HI\":PRINT#1:CLOSE 1|10 PRINT \"A\"|OPEN 2,3:CMD 2:LIST|PRINT#2:CLOSE 2|"),
        [
            "OPEN 1,3:CMD 1:PRINT \"HI\":PRINT#1:CLOSE",
            "1",
            "",
            "HI",
            "",
            "",
            "READY.",
            "10 PRINT \"A\"",
            "OPEN 2,3:CMD 2:LIST",
            "",
            "",
            "10 PRINT \"A\"",
            "READY.",
            "PRINT#2:CLOSE 2",
            "",
            "READY.",
        ]
    );
}

#[test]
fn cmd_to_an_absent_printer() {
    assert_eq!(
        screen("OPEN 4,4:CMD 4|"),
        ["OPEN 4,4:CMD 4", "", "?DEVICE NOT PRESENT  ERROR", "READY."]
    );
}

#[test]
fn under_cmd_a_line_entered_prints_no_new_line() {
    assert_eq!(
        screen("OPEN 1,3:CMD 1|PRINT \"X\";|PRINT \"Y\"|10 PRINT|LIST|POKE 19,0|PRINT \"Z\"|"),
        [
            "OPEN 1,3:CMD 1",
            "",
            "",
            "READY.",
            "PRINT \"X\"; X",
            "READY.",
            "PRINT \"Y\" Y",
            "",
            "READY.",
            "10 PRINT LIST",
            "10 PRINT",
            "READY.",
            "POKE 19,0",
            "READY.",
            "PRINT \"Z\"",
            "Z",
            "",
            "READY.",
        ]
    );
}

#[test]
fn under_cmd_input_asks_with_no_question_mark() {
    assert_eq!(
        screen(
            "OPEN 1,3:CMD 1|10 INPUT \"N\";A:PRINT A*2|RUN|5|20 INPUT A,B:PRINT A+B|RUN 20|5|6|"
        ),
        [
            "OPEN 1,3:CMD 1",
            "",
            "",
            "READY.",
            "10 INPUT \"N\";A:PRINT A*2 RUN N5  10",
            "",
            "READY.",
            "20 INPUT A,B:PRINT A+B RUN 20 5 6  11",
            "",
            "READY.",
        ]
    );
}

#[test]
fn under_cmd_a_reply_that_is_not_a_number_is_file_data() {
    assert_eq!(
        screen("OPEN 1,3:CMD 1|10 INPUT A:PRINT A|RUN|X|"),
        [
            "OPEN 1,3:CMD 1",
            "",
            "",
            "READY.",
            "10 INPUT A:PRINT A RUN X",
            "?FILE DATA  ERROR IN 10",
            "READY.",
        ]
    );
}

#[test]
fn a_line_feed_shows_nothing_on_the_screen() {
    assert_eq!(
        screen("PRINT \"A\";CHR$(10);\"B\"|"),
        ["PRINT \"A\";CHR$(10);\"B\"", "AB", "", "READY.",]
    );
}

#[test]
fn ready_goes_to_the_cmd_file_with_line_feeds() {
    assert_eq!(
        screen_after("OPEN 2,2,0,CHR$(6):CMD 2:PRINT \"A\"|POKE 19,0:SYS 65484:FOR I=0 TO 11:PRINT PEEK(PEEK(249)+256*PEEK(250)+I);:NEXT|", 300),
        [
            "OPEN 2,2,0,CHR$(6):CMD 2:PRINT \"A\"",
            "POKE 19,0:SYS 65484:FOR I=0 TO 11:PRINT",
            "PEEK(PEEK(249)+256*PEEK(250)+I);:NEXT",
            " 13  65  13  13  10  82  69  65  68  89",
            " 46  13",
            "READY.",
        ]
    );
}

#[test]
fn break_goes_to_the_cmd_file() {
    assert_eq!(
        screen_after("OPEN 2,2,0,CHR$(6):CMD 2:STOP|POKE 19,0:SYS 65484:B=PEEK(249)+256*PEEK(250):PRINT PEEK(670)|FOR I=0 TO 19:PRINT PEEK(B+I);:NEXT|", 300),
        [
            "OPEN 2,2,0,CHR$(6):CMD 2:STOP",
            "POKE 19,0:SYS 65484:B=PEEK(249)+256*PEEK",
            "(250):PRINT PEEK(670)",
            " 18",
            "",
            "READY.",
            "FOR I=0 TO 19:PRINT PEEK(B+I);:NEXT",
            " 13  13  10  66  82  69  65  75  13  10",
            " 82  69  65  68  89  46  13  10  0  0",
            "READY.",
        ]
    );
}

#[test]
fn ready_leaves_an_output_set_without_cmd() {
    assert_eq!(
        screen_after("OPEN 2,2,0,CHR$(6):POKE 781,2:SYS 65481:PRINT \"A\"|SYS 65484:B=PEEK(249)+256*PEEK(250):PRINT PEEK(670)|FOR I=0 TO 13:PRINT PEEK(B+I);:NEXT|", 300),
        [
            "OPEN 2,2,0,CHR$(6):POKE 781,2:SYS 65481:",
            "PRINT \"A\"",
            "SYS 65484:B=PEEK(249)+256*PEEK(250):PRIN",
            "T PEEK(670)",
            " 13",
            "",
            "READY.",
            "FOR I=0 TO 13:PRINT PEEK(B+I);:NEXT",
            " 65  13  13  10  82  69  65  68  89  46",
            " 13  10  13  0",
            "READY.",
        ]
    );
}

#[test]
fn an_error_brings_output_back_from_the_cmd_file() {
    assert_eq!(
        screen_after("OPEN 2,2,0,CHR$(6):CMD 2:PRINT )|PRINT PEEK(19)|", 200),
        [
            "OPEN 2,2,0,CHR$(6):CMD 2:PRINT )",
            "",
            "?SYNTAX  ERROR",
            "READY.",
            "PRINT PEEK(19)",
            " 0",
            "",
            "READY.",
        ]
    );
}

#[test]
fn entering_a_line_clr_and_run_close_the_files() {
    assert_eq!(
        screen_after("OPEN 1,3|10 PRINT#1,\"X\"|GOTO 10|OPEN 1,3:CLR:PRINT#1,\"Y\"|OPEN 1,3|RUN|PRINT#1,\"Z\"|", 150),
        [
            "OPEN 1,3",
            "",
            "READY.",
            "10 PRINT#1,\"X\"",
            "GOTO 10",
            "",
            "?FILE NOT OPEN  ERROR IN 10",
            "READY.",
            "OPEN 1,3:CLR:PRINT#1,\"Y\"",
            "",
            "?FILE NOT OPEN  ERROR",
            "READY.",
            "OPEN 1,3",
            "",
            "READY.",
            "RUN",
            "",
            "?FILE NOT OPEN  ERROR IN 10",
            "READY.",
            "PRINT#1,\"Z\"",
            "",
            "?FILE NOT OPEN  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn the_warm_start_ends_cmd() {
    assert_eq!(
        screen_after("OPEN 1,3:CMD 1|POKE 828,0:SYS 828|PRINT PEEK(19)|", 200),
        ["", "READY.", "PRINT PEEK(19)", " 0", "", "READY.",]
    );
}

#[test]
fn poke_and_wait_keep_their_address_through_a_peek() {
    assert_eq!(
        screen("POKE 828,7:POKE 829,PEEK(828):PRINT PEEK(829)|POKE 830,1:WAIT 830,PEEK(830):PRINT \"W\"|"),
        [
            "POKE 828,7:POKE 829,PEEK(828):PRINT PEEK",
            "(829)",
            " 7",
            "",
            "READY.",
            "POKE 830,1:WAIT 830,PEEK(830):PRINT \"W\"",
            "W",
            "",
            "READY.",
        ]
    );
}

#[test]
fn st_is_a_signed_byte() {
    assert_eq!(
        screen("POKE 144,64:PRINT ST|POKE 144,255:PRINT ST|POKE 144,127:PRINT ST|"),
        [
            "POKE 144,64:PRINT ST",
            " 64",
            "",
            "READY.",
            "POKE 144,255:PRINT ST",
            "-1",
            "",
            "READY.",
            "POKE 144,127:PRINT ST",
            " 127",
            "",
            "READY.",
        ]
    );
}

#[test]
fn opening_and_closing_rs232_clears_the_variables_and_moves_the_top() {
    assert_eq!(
        screen_after("A=5:A$=\"X\":OPEN 1,3:PRINT A;A$:CLOSE 1|A=5:A$=\"X\":OPEN 2,2,0,CHR$(6):PRINT A;A$;PEEK(55);PEEK(56)|A=7:CLOSE 2:PRINT A;PEEK(55);PEEK(56);PEEK(643);PEEK(644)|", 200),
        [
            "A=5:A$=\"X\":OPEN 1,3:PRINT A;A$:CLOSE 1",
            " 5 X",
            "",
            "READY.",
            "A=5:A$=\"X\":OPEN 2,2,0,CHR$(6):PRINT A;A$",
            ";PEEK(55);PEEK(56)",
            " 0  0  158",
            "",
            "READY.",
            "A=7:CLOSE 2:PRINT A;PEEK(55);PEEK(56);PE",
            "EK(643);PEEK(644)",
            " 0  0  160  0  160",
            "",
            "READY.",
        ]
    );
}

#[test]
fn a_second_rs232_file_and_its_close_clear_too() {
    assert_eq!(
        screen_after("OPEN 2,2,0,CHR$(6)|A=5:OPEN 3,2,0,CHR$(6):PRINT A;PEEK(55);PEEK(56);PEEK(643);PEEK(644)|A=6:CLOSE 3:PRINT A;PEEK(55);PEEK(56);PEEK(643);PEEK(644)|A=7:CLOSE 2:PRINT A;PEEK(55);PEEK(56);PEEK(643);PEEK(644)|", 250),
        [
            "OPEN 2,2,0,CHR$(6)",
            "",
            "READY.",
            "A=5:OPEN 3,2,0,CHR$(6):PRINT A;PEEK(55);",
            "PEEK(56);PEEK(643);PEEK(644)",
            " 0  0  158  0  158",
            "",
            "READY.",
            "A=6:CLOSE 3:PRINT A;PEEK(55);PEEK(56);PE",
            "EK(643);PEEK(644)",
            " 0  0  160  0  160",
            "",
            "READY.",
            "A=7:CLOSE 2:PRINT A;PEEK(55);PEEK(56);PE",
            "EK(643);PEEK(644)",
            " 0  0  160  0  160",
            "",
            "READY.",
        ]
    );
}

#[test]
fn an_rs232_open_empties_the_for_stack() {
    assert_eq!(
        screen_after(
            "10 FOR I=1 TO 2:OPEN 2,2,0,CHR$(6):PRINT I:CLOSE 2:NEXT|RUN|",
            250
        ),
        [
            "10 FOR I=1 TO 2:OPEN 2,2,0,CHR$(6):PRINT",
            " I:CLOSE 2:NEXT",
            "RUN",
            " 0",
            "",
            "?NEXT WITHOUT FOR  ERROR IN 10",
            "READY.",
        ]
    );
}

#[test]
fn an_rs232_open_restores_data() {
    assert_eq!(
        screen_after(
            "10 READ A:OPEN 2,2,0,CHR$(6):READ B:PRINT A;B:CLOSE 2|20 DATA 1,2|RUN|",
            250
        ),
        [
            "10 READ A:OPEN 2,2,0,CHR$(6):READ B:PRIN",
            "T A;B:CLOSE 2",
            "20 DATA 1,2",
            "RUN",
            " 0  1",
            "",
            "READY.",
        ]
    );
}

#[test]
fn an_rs232_open_empties_the_gosub_stack() {
    assert_eq!(
        screen_after(
            "10 GOSUB 20:PRINT \"BACK\":END|20 OPEN 2,2,0,CHR$(6):CLOSE 2:RETURN|RUN|",
            250
        ),
        [
            "10 GOSUB 20:PRINT \"BACK\":END",
            "20 OPEN 2,2,0,CHR$(6):CLOSE 2:RETURN",
            "RUN",
            "",
            "?RETURN WITHOUT GOSUB  ERROR IN 20",
            "READY.",
        ]
    );
}

#[test]
fn after_an_rs232_open_there_is_nothing_to_continue() {
    assert_eq!(
        screen_after("10 STOP:PRINT \"ON\"|RUN|OPEN 2,2,0,CHR$(6)|CONT|", 250),
        [
            "10 STOP:PRINT \"ON\"",
            "RUN",
            "",
            "BREAK IN 10",
            "READY.",
            "OPEN 2,2,0,CHR$(6)",
            "",
            "READY.",
            "CONT",
            "",
            "?CAN'T CONTINUE  ERROR",
            "READY.",
        ]
    );
}

#[test]
fn clr_leaves_the_rs232_buffers_taken() {
    assert_eq!(
        screen_after("OPEN 2,2,0,CHR$(6)|CLR:PRINT PEEK(55);PEEK(56);PEEK(643);PEEK(644);PEEK(248);PEEK(152)|", 250),
        [
            "OPEN 2,2,0,CHR$(6)",
            "",
            "READY.",
            "CLR:PRINT PEEK(55);PEEK(56);PEEK(643);PE",
            "EK(644);PEEK(248);PEEK(152)",
            " 0  158  0  158  159  0",
            "",
            "READY.",
        ]
    );
}

#[test]
fn return_moves_to_the_next_line_while_output_is_elsewhere() {
    assert_eq!(
        screen_after(
            "OPEN 2,2,0,CHR$(6):CMD 2|SYS 65484:POKE 19,0:PRINT \"X\"|",
            250
        ),
        [
            "OPEN 2,2,0,CHR$(6):CMD 2",
            "SYS 65484:POKE 19,0:PRINT \"X\"",
            "X",
            "",
            "READY.",
        ]
    );
}

#[test]
fn return_moves_to_the_next_line_under_chkout_too() {
    assert_eq!(
        screen_after(
            "OPEN 2,2,0,CHR$(6):POKE 781,2:SYS 65481|SYS 65484:PRINT \"Y\";|",
            250
        ),
        [
            "OPEN 2,2,0,CHR$(6):POKE 781,2:SYS 65481",
            "SYS 65484:PRINT \"Y\";",
            "Y",
            "READY.",
        ]
    );
}

#[test]
fn chkin_chkout_and_open_clear_st() {
    assert_eq!(
        screen_after("OPEN 2,0:OPEN 1,3|POKE 144,64:POKE 781,2:SYS 65478:B=ST:SYS 65484:PRINT B|POKE 144,64:POKE 781,1:SYS 65481:B=ST:SYS 65484:PRINT B|POKE 144,64:POKE 781,9:SYS 65478:B=ST:SYS 65484:PRINT B|POKE 144,64:OPEN 3,3:B=ST:PRINT B|", 400),
        [
            "OPEN 2,0:OPEN 1,3",
            "",
            "READY.",
            "POKE 144,64:POKE 781,2:SYS 65478:B=ST:SY",
            "S 65484:PRINT B",
            " 0",
            "",
            "READY.",
            "POKE 144,64:POKE 781,1:SYS 65481:B=ST:SY",
            "S 65484:PRINT B",
            " 0",
            "",
            "READY.",
            "POKE 144,64:POKE 781,9:SYS 65478:B=ST:SY",
            "S 65484:PRINT B",
            " 0",
            "",
            "READY.",
            "POKE 144,64:OPEN 3,3:B=ST:PRINT B",
            " 0",
            "",
            "READY.",
        ]
    );
}

#[test]
fn clrchn_close_and_a_refused_open_keep_st() {
    assert_eq!(
        screen_after("OPEN 1,3|POKE 144,64:SYS 65484:B=ST:PRINT B|POKE 144,64:CLOSE 1:B=ST:PRINT B|POKE 144,64:OPEN 0,3|PRINT ST|", 300),
        [
            "OPEN 1,3",
            "",
            "READY.",
            "POKE 144,64:SYS 65484:B=ST:PRINT B",
            " 64",
            "",
            "READY.",
            "POKE 144,64:CLOSE 1:B=ST:PRINT B",
            " 64",
            "",
            "READY.",
            "POKE 144,64:OPEN 0,3",
            "",
            "?NOT INPUT FILE  ERROR",
            "READY.",
            "PRINT ST",
            " 64",
            "",
            "READY.",
        ]
    );
}
