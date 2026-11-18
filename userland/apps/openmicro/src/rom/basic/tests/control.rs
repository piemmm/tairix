//! Control: loops, conditions, subroutines, procedures and functions,
//! DATA, and ON ERROR.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_openbasic::keyword::word;

use super::super::ws::page;
use super::{after, assert_screen, assert_screen_from, typed};

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

/// BASIC II mistakes an array's shape when an FN runs in a dimension but the
/// last, and ends a loop after one pass when its limit or step runs a FOR;
/// here each statement keeps its own.
#[test]
fn a_statement_keeps_its_own_through_an_fn_that_runs_dim_or_for() {
    assert_screen_from(
        &[
            "10 DEF FNI(N)=N*N DIV N",
            "20 DEF FNC(N):FOR J%=1 TO 2:NEXT:=N",
            "30 DIM G%(FNI(2),FNC(1)):G%(2,1)=5:PRINT G%(2,1)",
            "40 FOR I%=1 TO FNC(3) STEP FNC(1):PRINT I%;:NEXT:PRINT",
            "RUN",
        ],
        150,
        ">RUN",
        &[">RUN", "         5", "         1         2         3", ">"],
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

/// A true IF with no statements of its own, THEN or not, ends where its ELSE
/// part begins, and a false one runs that part.
#[test]
fn an_if_with_nothing_before_its_else() {
    assert_screen(
        &[
            "10 IF 1 ELSE PRINT \"N1\"",
            "20 IF 0 ELSE PRINT \"Y2\"",
            "30 IF 1 THEN ELSE PRINT \"N3\"",
            "40 IF 0 THEN ELSE PRINT \"Y4\"",
            "50 PRINT \"END\"",
            "RUN",
        ],
        60,
        &[
            ">10 IF 1 ELSE PRINT \"N1\"",
            ">20 IF 0 ELSE PRINT \"Y2\"",
            ">30 IF 1 THEN ELSE PRINT \"N3\"",
            ">40 IF 0 THEN ELSE PRINT \"Y4\"",
            ">50 PRINT \"END\"",
            ">RUN",
            "Y2",
            "Y4",
            "END",
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

/// The regression: a level of an FN recursing with a real argument took
/// more of the BASIC stack than BASIC II's, which takes 39 bytes, and 37
/// with an integer, so it ran out of room shallower.
#[test]
fn a_recursing_function_takes_no_more_room_a_level_than_basic_iis() {
    let screen = after(
        &[
            "10 D%=0:PRINT FNR(1.5):D%=0:PRINT FNI(1)",
            "20 PRINT \"R\";(S%-T%)/10;\" I\";(U%-V%)/10",
            "30 END",
            "40 DEF FNR(N):D%=D%+1:IF D%=10 S%=!4 AND &FFFF",
            "50 IF D%=20 T%=!4 AND &FFFF:=0",
            "60 =N+FNR(N+0.5)",
            "70 DEF FNI(N%):D%=D%+1:IF D%=10 U%=!4 AND &FFFF",
            "80 IF D%=20 V%=!4 AND &FFFF:=0",
            "90 =N%+FNI(N%+1)",
            "RUN",
        ],
        100,
    );
    let row = screen
        .iter()
        .find(|row| row.starts_with('R'))
        .expect("the bytes a level are printed");
    let bytes: Vec<u32> = row
        .split(['R', 'I'])
        .filter_map(|field| field.trim().parse().ok())
        .collect();
    assert_eq!(bytes.len(), 2, "{row}");
    assert!(bytes[0] <= 39 && bytes[1] <= 37, "{row}");
}

#[test]
fn array_elements_are_formals_and_locals_any_basic_their_subscripts() {
    assert_screen_from(
        &[
            "10 DIM A%(3),B(2):A%(2)=5:B(1)=1.5",
            "20 PROCP(A%(2),B(1)):PRINT A%(1);B(2)",
            "30 PROCQ(4):PRINT A%(1);B(2)",
            "40 PRINT FNR(2,3);A%(3)",
            "50 FOR I%=FNI(1) TO FNI(3) STEP FNI(1):PRINT I%;:NEXT:PRINT",
            "60 END",
            "70 DEF PROCP(A%(FNI(1)),B(FNI(2))):PRINT A%(1);B(2):ENDPROC",
            "80 DEF PROCQ(N):LOCAL A%(1),B(FNI(2)):A%(1)=N:B(2)=N*2:PRINT A%(1);B(2):ENDPROC",
            "90 DEF FNR(X,A%(X+1)):=X*A%(3)",
            "100 DEF FNI(N)=N*N DIV N",
            "RUN",
        ],
        150,
        ">RUN",
        &[
            ">RUN",
            "         51.5",
            "         00",
            "         48",
            "         00",
            "         00",
            "         1         2         3",
            ">",
        ],
    );
}

/// The regression: a call whose name ended a line of the longest length,
/// 255 bytes, left the statement pointer at the line's start, so the line
/// ran again, where BASIC II's goes on past it.
#[test]
fn a_call_ending_a_line_of_the_longest_length_returns_past_it() {
    let fn_call = [b'Q', b'=', word::FN.token, b'A'];
    for tail in [&[word::PROC.token, b'P'][..], &fn_call[..]] {
        // Line 100, built over the end marker: C% counted, PRINT of X's, then
        // the call filling the line's 255 bytes from its CR.
        let quote_at = 255 - tail.len() - 2;
        let mut pokes = vec![format!(
            "20 A%=TOP-2:?A%=13:A%?1=0:A%?2=100:A%?3=255:$(A%+4)=\"C%=C%+1:\":A%?12={}:A%?13=34",
            word::PRINT.token
        )];
        let mut tail_pokes = format!("31 A%?{quote_at}=34:A%?{}=58", quote_at + 1);
        for (at, byte) in (quote_at + 2..).zip(tail) {
            write!(tail_pokes, ":A%?{at}={byte}").expect("a String takes any text");
        }
        pokes.push(format!("30 FORI%=14TO{}:A%?I%=88:NEXT", quote_at - 1));
        pokes.push(tail_pokes + ":A%?255=13:A%?256=&FF");
        let lines: Vec<&str> = ["10 LOMEM=&5000:C%=0"]
            .into_iter()
            .chain(pokes.iter().map(String::as_str))
            .chain([
                "40 GOTO100",
                "50 DEFPROCP:IFC%>3 END",
                "60 ENDPROC",
                "70 DEFFNA:IFC%>3 END",
                "80 =3",
                "RUN",
            ])
            .collect();
        let machine = typed(&lines, 300);
        assert_eq!(machine.peek(page::resident(b'C')), 1, "{tail:?} once");
    }
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

/// EVAL, a READ item and VAL evaluate text away from the statement, and an
/// error there is the statement's line, or none typed; an FN's body is its
/// own line.
#[test]
fn an_error_in_text_borrowed_from_a_statement_is_at_the_statement_s_line() {
    assert_screen(
        &[
            "X=EVAL(\"1/0\")",
            "PRINT VAL(\"1E99\")",
            "5 N%=0",
            "10 ON ERROR PRINT ERL;:N%=N%+1:ON N% GOTO 30,40,50,60,70",
            "20 X=EVAL(\"1/0\")",
            "30 READ Q",
            "40 X=EVAL(\"FNA\")",
            "50 X=FNB",
            "60 X=FNC",
            "70 END",
            "100 DEF FNA=1/0",
            "110 DEF FNB=EVAL(\"1/0\")",
            "120 DEF FNC:READ R:=R",
            "200 DATA 1/0",
            "RUN",
            "200 DATA EVAL(\"1/0\")",
            "RUN",
        ],
        100,
        &[
            ">X=EVAL(\"1/0\")",
            "",
            "Division by zero",
            ">PRINT VAL(\"1E99\")",
            "",
            "Too big",
            ">5 N%=0",
            ">10 ON ERROR PRINT ERL;:N%=N%+1:ON N% GO",
            "TO 30,40,50,60,70",
            ">20 X=EVAL(\"1/0\")",
            ">30 READ Q",
            ">40 X=EVAL(\"FNA\")",
            ">50 X=FNB",
            ">60 X=FNC",
            ">70 END",
            ">100 DEF FNA=1/0",
            ">110 DEF FNB=EVAL(\"1/0\")",
            ">120 DEF FNC:READ R:=R",
            ">200 DATA 1/0",
            ">RUN",
            "        20        30       100       110",
            "       120>200 DATA EVAL(\"1/0\")",
            ">RUN",
            "        20        30       100       110",
            "       120>",
        ],
    );
}
