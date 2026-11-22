//! The interpreter run on programs: statements, expressions, routines and
//! the machine's resumption, held to what BASIC V gives.

mod common;

use common::basic;

#[test]
fn a_program_prints_the_value_of_an_expression() {
    assert_eq!(basic("PRINT 1+2\n"), "         3\n");
}

#[test]
fn variables_hold_integers_reals_and_strings() {
    let out = basic("A%=7:b=2.5:c$=\"hi\"\nPRINT A%;b;c$\nPRINT A%*b\n");
    assert_eq!(
        out, "         72.5hi\n      17.5\n",
        "the first number is padded"
    );
}

#[test]
fn loops_count_and_end_where_they_should() {
    let out = basic(
        "FOR I%=1 TO 3:PRINT I%;:NEXT\nPRINT\nN%=0:REPEAT N%+=1:UNTIL N%=5:PRINT N%\nW%=3:WHILE W%>0:W%-=1:ENDWHILE:PRINT W%\n",
    );
    assert_eq!(
        out,
        "         1         2         3\n         5\n         0\n"
    );
}

#[test]
fn if_takes_its_then_and_else_parts_and_blocks() {
    let out = basic(
        "IF 1 THEN PRINT \"a\" ELSE PRINT \"b\"\nIF 0 THEN PRINT \"c\" ELSE PRINT \"d\"\nIF 1 THEN\nPRINT \"e\"\nELSE\nPRINT \"f\"\nENDIF\nIF 0 THEN\nPRINT \"g\"\nELSE\nPRINT \"h\"\nENDIF\n",
    );
    assert_eq!(out, "a\nd\ne\nh\n");
}

#[test]
fn procedures_and_functions_take_parameters_and_restore_locals() {
    let out = basic(
        "x=1:PROCp(5):PRINT x\nPRINT FNsq(4)+1\nPRINT FNfact(5)\nEND\nDEF PROCp(x):PRINT x:ENDPROC\nDEF FNsq(n)=n*n\nDEF FNfact(n):IF n<=1 THEN =1\n=n*FNfact(n-1)\n",
    );
    assert_eq!(out, "         5\n         1\n        17\n       120\n");
}

#[test]
fn arrays_hold_elements_and_strings_slice() {
    let out = basic(
        "DIM a%(3),s$(2)\nFOR i%=0 TO 3:a%(i%)=i%*i%:NEXT\nPRINT a%(3)\ns$(1)=\"abc\":PRINT s$(1);LEN(s$(1))\nPRINT LEFT$(\"hello\",2);MID$(\"hello\",2,3);RIGHT$(\"hello\",2)\n",
    );
    assert_eq!(out, "         9\nabc3\nheelllo\n");
}

#[test]
fn data_is_read_in_order_and_restored() {
    let out = basic("READ a,b$,c\nPRINT a;b$;c\nRESTORE:READ d:PRINT d\nDATA 1,two, 3\n");
    assert_eq!(out, "         1two3\n         1\n");
}

#[test]
fn an_error_reaches_on_error_with_its_number_and_line() {
    let out = basic("ON ERROR PRINT ERR;\" \";ERL:END\nPRINT 1/0\n");
    assert_eq!(out, "        18 20\n", "a text program numbered from ten");
}

#[test]
fn dim_takes_its_names_after_spaces() {
    let out = basic("DIM x% 10, a%( 3 )\nDIM  b(1)\nPRINT DIM(a%(),1)\nPRINT DIM(b(),1)\n");
    assert_eq!(out, "         3\n         1\n");
}

#[test]
fn rnd_draws_as_genuine_basic_does() {
    // Recorded on RISC OS 5 after RND(-1).
    let out =
        basic("X=RND(-1):PRINT RND(2);\" \";RND(1000);\" \";RND(&7FFFFFFF);\" \";RND(65536)\n");
    assert_eq!(out, "         2 223 278888256 14144\n");
}

#[test]
fn a_number_alone_in_a_text_program_stores_no_line() {
    let out = basic("10 A=1  \n20 PRINT A\n30\n");
    assert_eq!(out, "         1\n");
}

#[test]
fn sys_passes_a_string_in_any_register_the_first_included() {
    assert_eq!(
        basic("10 SYS \"OS_Write0\",\"first\"\n20 PRINT \" then\"\n"),
        "first then\n"
    );
    assert_eq!(basic("10 R%=7:SYS &2,\"x\":PRINT R%\n"), "x         7\n");
}
