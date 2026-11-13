//! Values: indirection, the operators, strings and their functions, and
//! arrays.

use super::assert_screen;

#[test]
fn indirection_string_comparison_and_hex() {
    assert_screen(
        &[
            "?&3000=5:?&3001=9:A=&2FFF:PRINT A?1+1",
            "PRINT )",
            "PRINT AND",
            "PRINT \"A\"<\"B\",\"AB\">\"A\",\"A\"=\"A\"",
            "PRINT \"A\"+1",
            "PRINT 1+",
            "PRINT &123456789,&FFFFFFFF",
            "PRINT &",
        ],
        50,
        &[
            ">?&3000=5:?&3001=9:A=&2FFF:PRINT A?1+1",
            "         6",
            ">PRINT )",
            "",
            "No such variable",
            ">PRINT AND",
            "",
            "No such variable",
            ">PRINT \"A\"<\"B\",\"AB\">\"A\",\"A\"=\"A\"",
            "        -1        -1        -1",
            ">PRINT \"A\"+1",
            "",
            "Type mismatch",
            ">PRINT 1+",
            "",
            "No such variable",
            ">PRINT &123456789,&FFFFFFFF",
            " 591751049        -1",
            ">PRINT &",
            "",
            "Bad HEX",
            ">",
        ],
    );
}

#[test]
fn integer_operators() {
    assert_screen(
        &[
            "PRINT 3*4,-3*4,300*300,-65536*65536,65535*65535,100000*3,-7*-8",
            "PRINT 7 DIV 2, -7 DIV 2, -7 MOD 2, 7 MOD -2",
            "PRINT 12 AND 10, 12 OR 3, 12 EOR 5, 2.5 DIV 1, 7.9 MOD 2",
            "PRINT 1 AND 3.7, NOT 0",
        ],
        40,
        &[
            ">PRINT 3*4,-3*4,300*300,-65536*65536,655",
            "35*65535,100000*3,-7*-8",
            "        12       -12     90000-4.2949673",
            "E9        4.29483622E9            300000",
            "        56",
            ">PRINT 7 DIV 2, -7 DIV 2, -7 MOD 2, 7 MO",
            "D -2",
            "         3        -3        -1         1",
            "",
            ">PRINT 12 AND 10, 12 OR 3, 12 EOR 5, 2.5",
            " DIV 1, 7.9 MOD 2",
            "         8        15         9         2",
            "         1",
            ">PRINT 1 AND 3.7, NOT 0",
            "         1        -1",
            ">",
        ],
    );
}

#[test]
fn string_slices_and_searches() {
    assert_screen(
        &["A$=\"HELLO\":PRINT LEFT$(A$,2);\"|\";LEFT$(A$,9);\"|\";RIGHT$(A$,2);\"|\";RIGHT$(A$,9);\"|\";LEFT$(A$,0)", "PRINT MID$(A$,2,2);\"|\";MID$(A$,2);\"|\";MID$(A$,9);\"|\";MID$(A$,0,2);\"|\";MID$(A$,5,9)", "PRINT INSTR(A$,\"L\"),INSTR(A$,\"L\",4),INSTR(A$,\"\"),INSTR(A$,\"X\"),INSTR(A$,\"L\",9)"],
        60,
        &[
            ">A$=\"HELLO\":PRINT LEFT$(A$,2);\"|\";LEFT$(",
            "A$,9);\"|\";RIGHT$(A$,2);\"|\";RIGHT$(A$,9);",
            "\"|\";LEFT$(A$,0)",
            "HE|HELLO|LO|HELLO|",
            ">PRINT MID$(A$,2,2);\"|\";MID$(A$,2);\"|\";M",
            "ID$(A$,9);\"|\";MID$(A$,0,2);\"|\";MID$(A$,5",
            ",9)",
            "EL|ELLO||HE|O",
            ">PRINT INSTR(A$,\"L\"),INSTR(A$,\"L\",4),INS",
            "TR(A$,\"\"),INSTR(A$,\"X\"),INSTR(A$,\"L\",9)",
            "         3         4         1         0",
            "         0",
            ">",
        ],
    );
}

#[test]
fn string_codes_values_and_evaluation() {
    assert_screen(
        &[
            "A$=\"HELLO\":PRINT ASC(\"\"),ASC(\"AB\"),LEN(\"\"),SGN(-3),SGN(0),SGN(2.5)",
            "PRINT VAL(\"  -12.5E1X\"),VAL(\"\"),VAL(\"+3\"),VAL(\"&10\"),VAL(\"-\")",
            "PRINT STRING$(3,\"AB\");\"|\";STRING$(0,\"X\");\"|\";CHR$(65);CHR$(321)",
            "PRINT EVAL(\"1+2*3\"),EVAL(\"A$\")",
        ],
        60,
        &[
            ">A$=\"HELLO\":PRINT ASC(\"\"),ASC(\"AB\"),LEN(",
            "\"\"),SGN(-3),SGN(0),SGN(2.5)",
            "        -1        65         0        -1",
            "         0         1",
            ">PRINT VAL(\"  -12.5E1X\"),VAL(\"\"),VAL(\"+3",
            "\"),VAL(\"&10\"),VAL(\"-\")",
            "      -125         0         3         0",
            "         0",
            ">PRINT STRING$(3,\"AB\");\"|\";STRING$(0,\"X\"",
            ");\"|\";CHR$(65);CHR$(321)",
            "ABABAB||AA",
            ">PRINT EVAL(\"1+2*3\"),EVAL(\"A$\")",
            "         7HELLO",
            ">",
        ],
    );
}

#[test]
fn string_function_arguments_at_their_limits() {
    assert_screen(
        &["A$=\"HELLO\":PRINT LEFT$(A$,-1);\"|\";RIGHT$(A$,-1);\"|\";MID$(A$,-1,2);\"|\";MID$(A$,2,-1);\"|\";LEFT$(A$,256);\"|\";LEFT$(A$,257)", "PRINT STRING$(-1,\"A\");\"|\";STRING$(256,\"A\");\"|\"", "PRINT INSTR(\"ABAB\",\"B\",0),INSTR(\"ABAB\",\"\",3),INSTR(\"\",\"\",1),INSTR(\"AB\",\"\",5)", "PRINT MID$(A$,256,2);\"|\";MID$(A$,6)"],
        60,
        &[
            ">A$=\"HELLO\":PRINT LEFT$(A$,-1);\"|\";RIGHT",
            "$(A$,-1);\"|\";MID$(A$,-1,2);\"|\";MID$(A$,2",
            ",-1);\"|\";LEFT$(A$,256);\"|\";LEFT$(A$,257)",
            "",
            "HELLO|HELLO||ELLO||H",
            ">PRINT STRING$(-1,\"A\");\"|\";STRING$(256,\"",
            "A\");\"|\"",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAA||",
            ">PRINT INSTR(\"ABAB\",\"B\",0),INSTR(\"ABAB\",",
            "\"\",3),INSTR(\"\",\"\",1),INSTR(\"AB\",\"\",5)",
            "         2         3         1         0",
            "",
            ">PRINT MID$(A$,256,2);\"|\";MID$(A$,6)",
            "HE|",
            ">",
        ],
    );
}

#[test]
fn arrays_and_their_errors() {
    assert_screen(
        &[
            "DIM A(3):PRINT A(1,2)",
            "DIM B(2,2):PRINT B(1)",
            "PRINT A(4)",
            "PRINT A(-1)",
            "DIM A(2)",
            "DIM C(-1)",
            "DIM D%(0):D%(0)=5:PRINT D%(0)",
            "DIM P% 10:PRINT P%-TOP>0",
        ],
        50,
        &[
            ">DIM A(3):PRINT A(1,2)",
            "",
            "Missing )",
            ">DIM B(2,2):PRINT B(1)",
            "",
            "Array",
            ">PRINT A(4)",
            "",
            "Subscript",
            ">PRINT A(-1)",
            "",
            "Subscript",
            ">DIM A(2)",
            "",
            "Bad DIM",
            ">DIM C(-1)",
            "",
            "Bad DIM",
            ">DIM D%(0):D%(0)=5:PRINT D%(0)",
            "         5",
            ">DIM P% 10:PRINT P%-TOP>0",
            "        -1",
            ">",
        ],
    );
}

#[test]
fn arrays_of_each_type_hold_their_values() {
    assert_screen(
        &[
            "DIM A(3,4), B%(2) ,C$(1)",
            "A(1,2)=7:B%(2)=5:C$(1)=\"Z\":A(3,4)=-1.5",
            "PRINT A(1,2);B%(2);C$(1);A(3,4);A(0,0);B%(0);C$(0);\"|\"",
            "DIM E(2):E(1)=E(1)+3:PRINT E(1)",
        ],
        50,
        &[
            ">DIM A(3,4), B%(2) ,C$(1)",
            ">A(1,2)=7:B%(2)=5:C$(1)=\"Z\":A(3,4)=-1.5",
            ">PRINT A(1,2);B%(2);C$(1);A(3,4);A(0,0);",
            "B%(0);C$(0);\"|\"",
            "         75Z-1.500|",
            ">DIM E(2):E(1)=E(1)+3:PRINT E(1)",
            "         3",
            ">",
        ],
    );
}

#[test]
fn a_string_given_room_gets_basic_ii_s_slack() {
    // From 8 characters on, 8 more than its length, at most 255, whether
    // made, moved or grown where it lies.
    assert_screen(
        &[
            "CLEAR:P%=!2:B$=STRING$(7,\"Q\"):PRINT (!2-P%) AND &FFFF",
            "CLEAR:P%=!2:B$=STRING$(8,\"Q\"):PRINT (!2-P%) AND &FFFF",
            "CLEAR:P%=!2:B$=STRING$(250,\"Q\"):PRINT (!2-P%) AND &FFFF",
            "CLEAR:A$=\"ABC\":X=1:P%=!2:A$=\"ABCDEFGHIJ\":PRINT (!2-P%) AND &FFFF",
            "P%=!2:A$=\"ABCDEFGHIJKLMNOPQR\":PRINT (!2-P%) AND &FFFF",
            "CLEAR:A$=\"ABCDEFGHIJ\":P%=!2:A$=STRING$(19,\"R\"):PRINT (!2-P%) AND &FFFF",
        ],
        100,
        &[
            ">CLEAR:P%=!2:B$=STRING$(7,\"Q\"):PRINT (!2",
            "-P%) AND &FFFF",
            "        15",
            ">CLEAR:P%=!2:B$=STRING$(8,\"Q\"):PRINT (!2",
            "-P%) AND &FFFF",
            "        24",
            ">CLEAR:P%=!2:B$=STRING$(250,\"Q\"):PRINT (",
            "!2-P%) AND &FFFF",
            "       263",
            ">CLEAR:A$=\"ABC\":X=1:P%=!2:A$=\"ABCDEFGHIJ",
            "\":PRINT (!2-P%) AND &FFFF",
            "        18",
            ">P%=!2:A$=\"ABCDEFGHIJKLMNOPQR\":PRINT (!2",
            "-P%) AND &FFFF",
            "         0",
            ">CLEAR:A$=\"ABCDEFGHIJ\":P%=!2:A$=STRING$(",
            "19,\"R\"):PRINT (!2-P%) AND &FFFF",
            "         9",
            ">",
        ],
    );
}

#[test]
fn a_fixed_string_without_a_cr_in_256_bytes_is_empty() {
    assert_screen(
        &[
            "FOR I%=0 TO 300:I%?&A00=65:NEXT:PRINT LEN($&A00)",
            "?(&A00+255)=13:PRINT LEN($&A00)",
        ],
        60,
        &[
            ">FOR I%=0 TO 300:I%?&A00=65:NEXT:PRINT L",
            "EN($&A00)",
            "         0",
            ">?(&A00+255)=13:PRINT LEN($&A00)",
            "       255",
            ">",
        ],
    );
}
