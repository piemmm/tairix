//! Control: loops, conditions, subroutines, procedures and functions,
//! DATA, and ON ERROR.

use super::{assert_screen, assert_screen_from};

#[test]
fn read_takes_data_as_it_is_written() {
    assert_screen(
        &[
            "10 DATA 2*3, \" A B \",C D ,&10,\"Q\"\"Z\"",
            "20 READ A,B$,C$,D,E$:PRINT A;\"|\";B$;\"|\";C$;\"|\";D;\"|\";E$",
            "RUN",
        ],
        50,
        &[
            ">10 DATA 2*3, \" A B \",C D ,&10,\"Q\"\"Z\"",
            ">20 READ A,B$,C$,D,E$:PRINT A;\"|\";B$;\"|\"",
            ";C$;\"|\";D;\"|\";E$",
            ">RUN",
            "         6| A B |C D |16|Q\"Z",
            ">",
        ],
    );
}

#[test]
fn for_loops_run_and_end_as_basic_ii_runs_them() {
    assert_screen(
        &[
            "40 FOR J=1 TO 0:PRINT \"IN\":NEXT:PRINT J",
            "50 FOR K=1 TO 2 STEP 0.5:PRINT K;:NEXT:PRINT",
            "60 FOR L%=3 TO 1 STEP -1:PRINT L%;:NEXT:PRINT L%",
            "70 FOR M=0 TO 1 STEP 0.1:NEXT:PRINT M",
            "RUN",
        ],
        60,
        &[
            ">40 FOR J=1 TO 0:PRINT \"IN\":NEXT:PRINT J",
            "",
            ">50 FOR K=1 TO 2 STEP 0.5:PRINT K;:NEXT:",
            "PRINT",
            ">60 FOR L%=3 TO 1 STEP -1:PRINT L%;:NEXT",
            ":PRINT L%",
            ">70 FOR M=0 TO 1 STEP 0.1:NEXT:PRINT M",
            ">RUN",
            "IN",
            "         2",
            "         1       1.5         2",
            "         3         2         1         0",
            "",
            "         1",
            ">",
        ],
    );
}

#[test]
fn gosub_returns_and_trace_shows_lines() {
    assert_screen(
        &[
            "10 PRINT \"A\"",
            "20 GOSUB 40:PRINT \"C\"",
            "30 END",
            "40 PRINT \"B\":RETURN",
            "TRACE ON",
            "RUN",
            "TRACE 25:RUN",
            "TRACE OFF",
        ],
        60,
        &[
            ">10 PRINT \"A\"",
            ">20 GOSUB 40:PRINT \"C\"",
            ">30 END",
            ">40 PRINT \"B\":RETURN",
            ">TRACE ON",
            ">RUN",
            "[10] A",
            "[20] [40] B",
            "C",
            "[30] >TRACE 25:RUN",
            "[10] A",
            "[20] B",
            "C",
            ">TRACE OFF",
            ">",
        ],
    );
}

#[test]
fn repeat_if_else_and_on() {
    assert_screen(
        &[
            "10 I=0:REPEAT I=I+1:PRINT I;:UNTIL I=3",
            "20 IF I=3 THEN PRINT \"Y\" ELSE PRINT \"N\"",
            "30 IF I=4 PRINT \"Y\" ELSE PRINT \"N\"",
            "40 ON 2 GOSUB 70,80:PRINT \"B\"",
            "50 ON 5 GOTO 70,80 ELSE PRINT \"E\"",
            "60 END",
            "70 PRINT \"70\":RETURN",
            "80 PRINT \"80\":RETURN",
            "RUN",
        ],
        60,
        &[
            ">10 I=0:REPEAT I=I+1:PRINT I;:UNTIL I=3",
            ">20 IF I=3 THEN PRINT \"Y\" ELSE PRINT \"N\"",
            "",
            ">30 IF I=4 PRINT \"Y\" ELSE PRINT \"N\"",
            ">40 ON 2 GOSUB 70,80:PRINT \"B\"",
            ">50 ON 5 GOTO 70,80 ELSE PRINT \"E\"",
            ">60 END",
            ">70 PRINT \"70\":RETURN",
            ">80 PRINT \"80\":RETURN",
            ">RUN",
            "         1         2         3Y",
            "N",
            "80",
            "B",
            "E",
            ">",
        ],
    );
}

#[test]
fn restore_and_out_of_data() {
    assert_screen(
        &[
            "10 DATA 1,2",
            "20 DATA 3",
            "30 READ A,B,C,D",
            "RUN",
            "40 RESTORE 20:READ X:PRINT X",
            "RUN",
        ],
        60,
        &[
            ">10 DATA 1,2",
            ">20 DATA 3",
            ">30 READ A,B,C,D",
            ">RUN",
            "",
            "Out of DATA at line 30",
            ">40 RESTORE 20:READ X:PRINT X",
            ">RUN",
            "",
            "Out of DATA at line 30",
            ">",
        ],
    );
}

#[test]
fn procedures_bind_parameters_and_locals() {
    assert_screen(
        &[
            "10 A=1:B=2:PROCT(B,A):PRINT A;B",
            "20 PRINT FNF(3)+FNF(4);\" \";X$",
            "30 END",
            "40 DEF PROCT(A,B):PRINT A;B;\" \";:LOCAL A:A=9:PRINT A;\" \";:ENDPROC",
            "50 DEF FNF(N):LOCAL X$:X$=\"L\":=N*10",
            "60 X$=\"G\"",
            "RUN",
        ],
        60,
        &[
            ">10 A=1:B=2:PROCT(B,A):PRINT A;B",
            ">20 PRINT FNF(3)+FNF(4);\" \";X$",
            ">30 END",
            ">40 DEF PROCT(A,B):PRINT A;B;\" \";:LOCAL",
            "A:A=9:PRINT A;\" \";:ENDPROC",
            ">50 DEF FNF(N):LOCAL X$:X$=\"L\":=N*10",
            ">60 X$=\"G\"",
            ">RUN",
            "         21          9          12",
            "        70",
            ">",
        ],
    );
}

#[test]
fn procedures_recurse_and_take_strings() {
    assert_screen(
        &[
            "10 PRINT FNFACT(10), FNFACT(5)",
            "20 PROCS(\"AB\",3):PRINT FNR$(\"XYZ\")",
            "30 END",
            "40 DEF FNFACT(N):IF N<=1 THEN =1 ELSE =N*FNFACT(N-1)",
            "50 DEF PROCS(A$,N%):LOCAL I%:FOR I%=1 TO N%:PRINT A$;:NEXT:PRINT:ENDPROC",
            "60 DEF FNR$(S$):LOCAL R$,I%:FOR I%=LEN(S$) TO 1 STEP -1:R$=R$+MID$(S$,I%,1):NEXT:=R$",
            "RUN",
        ],
        80,
        &[
            ">10 PRINT FNFACT(10), FNFACT(5)",
            ">20 PROCS(\"AB\",3):PRINT FNR$(\"XYZ\")",
            ">30 END",
            ">40 DEF FNFACT(N):IF N<=1 THEN =1 ELSE =",
            "N*FNFACT(N-1)",
            ">50 DEF PROCS(A$,N%):LOCAL I%:FOR I%=1 T",
            "O N%:PRINT A$;:NEXT:PRINT:ENDPROC",
            ">60 DEF FNR$(S$):LOCAL R$,I%:FOR I%=LEN(",
            "S$) TO 1 STEP -1:R$=R$+MID$(S$,I%,1):NEX",
            "T:=R$",
            ">RUN",
            "   3628800       120",
            "ABABAB",
            "",
            "No such variable at line 60",
            ">",
        ],
    );
}

#[test]
fn procedure_errors() {
    assert_screen(
        &[
            "ENDPROC",
            "30 PROCB",
            "RUN",
            "30 PRINT FNC",
            "RUN",
            "10 DEF PROCA(X):PRINT X:ENDPROC",
            "20 PROCA(1):PROCA (2):PROCA(3):END",
            "RUN",
        ],
        60,
        &[
            ">ENDPROC",
            "",
            "No PROC",
            ">30 PROCB",
            ">RUN",
            "",
            "No such FN/PROC at line 30",
            ">30 PRINT FNC",
            ">RUN",
            "",
            "No such FN/PROC at line 30",
            ">10 DEF PROCA(X):PRINT X:ENDPROC",
            ">20 PROCA(1):PROCA (2):PROCA(3):END",
            ">RUN",
            "         1",
            "         2",
            "         3",
            ">",
        ],
    );
}

#[test]
fn return_without_gosub_and_unknown_variables() {
    assert_screen(
        &[
            "10 GOSUB 30:PRINT Q",
            "20 END",
            "30 RETURN",
            "40 PRINT 1:PRINT X",
            "RUN",
            "GOTO 40",
        ],
        50,
        &[
            ">10 GOSUB 30:PRINT Q",
            ">20 END",
            ">30 RETURN",
            ">40 PRINT 1:PRINT X",
            ">RUN",
            "",
            "No such variable at line 10",
            ">GOTO 40",
            "         1",
            "",
            "No such variable at line 40",
            ">",
        ],
    );
}

#[test]
fn on_error_runs_its_statements() {
    assert_screen(
        &[
            "PRINT\"HI\";",
            "VDU 65",
            "PRINT\"HI\";:VDU 66",
            "10 ON ERROR PRINT \"E\";ERR:END",
            "20 Q",
            "RUN",
            "30 ON ERROR OFF",
            "RUN",
        ],
        50,
        &[
            ">PRINT\"HI\";",
            "HI>VDU 65",
            "A>PRINT\"HI\";:VDU 66",
            "HIB>10 ON ERROR PRINT \"E\";ERR:END",
            ">20 Q",
            ">RUN",
            "E4",
            ">30 ON ERROR OFF",
            ">RUN",
            "E4",
            ">",
        ],
    );
}

#[test]
fn local_indirections_keep_what_basic_ii_keeps() {
    // A string at an address is cleared as a string variable's descriptor
    // is, its fourth byte made 0; a byte is neither kept nor put back.
    assert_screen_from(
        &[
            "10FOR I=0 TO 7:?(&3000+I)=&77:NEXT",
            "20$&3000=\"AB\"",
            "30PROCA",
            "40FOR I=0 TO 7:PRINT ~?(&3000+I);\" \";:NEXT:PRINT",
            "50END",
            "60DEFPROCA",
            "70LOCAL $&3000",
            "80FOR I=0 TO 7:PRINT ~?(&3000+I);\" \";:NEXT:PRINT",
            "90$&3000=\"XYZW\"",
            "100ENDPROC",
            "RUN",
        ],
        200,
        ">RUN",
        &[
            ">RUN",
            "        41         42          D",
            "  0         77         77         77",
            "     77",
            "        41         42          D",
            " 57          D         77         77",
            "     77",
            ">",
        ],
    );
    assert_screen_from(
        &[
            "10?&70=9:!&80=&12345678",
            "20PROCA",
            "30PRINT ?&70;\" \";~!&80",
            "40END",
            "50DEFPROCA",
            "60LOCAL ?&70,!&80",
            "70PRINT ?&70;\" \";~!&80",
            "80?&70=3:!&80=5",
            "90ENDPROC",
            "RUN",
        ],
        100,
        ">RUN",
        &[">RUN", "         9 0", "         3 12345678", ">"],
    );
}

#[test]
fn a_string_formal_at_an_address_is_put_back() {
    assert_screen_from(
        &[
            "10$&3000=\"ABCDEFG\"",
            "20PROCA(\"XY\")",
            "30PRINT $&3000",
            "40END",
            "50DEFPROCA($&3000)",
            "60PRINT $&3000",
            "80ENDPROC",
            "RUN",
        ],
        100,
        ">RUN",
        &[">RUN", "XY", "ABCDEFG", ">"],
    );
}

#[test]
fn data_starts_again_at_each_typed_line_and_at_clear() {
    assert_screen(
        &[
            "READ A%:PRINT A%",
            "10 READ A%:CLEAR:READ B%:PRINT A%;B%",
            "20 DATA 1,2,3",
            "RUN",
            "READ A%:PRINT A%",
            "READ A%,B%:PRINT A%;B%",
        ],
        100,
        &[
            ">READ A%:PRINT A%",
            "",
            "Out of DATA",
            ">10 READ A%:CLEAR:READ B%:PRINT A%;B%",
            ">20 DATA 1,2,3",
            ">RUN",
            "         11",
            ">READ A%:PRINT A%",
            "         1",
            ">READ A%,B%:PRINT A%;B%",
            "         12",
            ">",
        ],
    );
}
