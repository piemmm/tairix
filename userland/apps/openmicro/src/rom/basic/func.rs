//! Functions: the tokens from OPENIN to EOF in a factor, each with its
//! argument as a factor or bracketed list, as BASIC II takes them.
//!
//! A function returns its kind in A to the factor's end, which reads the
//! character after it; one whose argument is a factor gives that factor's
//! next character back first.

use super::float::{FAC, FRACTION};
use super::ws::{kind, page, zp as z};
use crate::rom::asm::byte;
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Acc, Asm, RTS};
use crate::rom::os::{entry, osbyte, ERROR_POINTER};

/// The first function token.
const FIRST: u8 = 0x8E;

/// Each function token's routine, from OPENIN.
const FUNCTIONS: [&str; 56] = [
    "fn_openin",
    "fn_ptr",
    "fn_page",
    "fn_time",
    "fn_lomem",
    "fn_himem",
    "fn_abs",
    "fn_acs",
    "fn_adval",
    "fn_asc",
    "fn_asn",
    "fn_atn",
    "fn_bget",
    "fn_cos",
    "fn_count",
    "fn_deg",
    "fn_erl",
    "fn_err",
    "fn_eval",
    "fn_exp",
    "fn_ext",
    "fn_false",
    "fn_fn",
    "fn_get",
    "fn_inkey",
    "fn_instr",
    "fn_int",
    "fn_len",
    "fn_ln",
    "fn_log",
    "fn_not",
    "fn_openup",
    "fn_openout",
    "fn_pi",
    "fn_point",
    "fn_pos",
    "fn_rad",
    "fn_rnd",
    "fn_sgn",
    "fn_sin",
    "fn_sqr",
    "fn_tan",
    "fn_top",
    "fn_true",
    "fn_usr",
    "fn_val",
    "fn_vpos",
    "fn_chr",
    "fn_get_string",
    "fn_inkey_string",
    "fn_left",
    "fn_mid",
    "fn_right",
    "fn_str",
    "fn_string",
    "fn_eof",
];

/// Where a part of a string starts.
const FROM_OFFSET: u16 = z::INT_WORK;
const CR: u8 = 0x0D;
/// LOMEM is zero page's first word, and a string's kind the 0 INKEY$
/// returns with nothing.
const _: () = assert!(z::LOMEM == 0 && kind::STRING == 0);

pub fn emit(a: &mut Asm) {
    dispatch(a);
    arguments(a);
    numeric(a);
    machine(a);
    devices(a);
    strings(a);
    slicing(a);
    searching(a);
    evaluating(a);
    string_cr(a);
    table(a);
}

fn dispatch(a: &mut Asm) {
    // A function token at a factor's start: its routine, returning to the
    // factor's end. Any other token there names no variable.
    a.here("factor_token");
    a.sec();
    a.sbc(imm(FIRST));
    a.cmp(imm(byte(FUNCTIONS.len())));
    let none = a.local();
    a.bcs(none);
    a.tax();
    a.lda(imm(hi(sym("factor_done") + -1)));
    a.pha();
    a.lda(imm(lo(sym("factor_done") + -1)));
    a.pha();
    a.lda(abs("function_high").x());
    a.pha();
    a.lda(abs("function_low").x());
    a.pha();
    a.rts();
    a.bind(none);
    a.jmp("err_no_such_variable");
}

fn arguments(a: &mut Asm) {
    // A factor as the argument, its next character given back: as it is,
    // a number, an integer, a real or a string.
    a.here("arg_number");
    a.jsr("arg_factor");
    a.bne(RTS);
    let mismatch = a.here_local();
    a.jmp("err_type_mismatch");

    a.here("arg_integer");
    a.jsr("arg_factor");
    a.jmp("make_integer");

    a.here("arg_real");
    a.jsr("arg_factor");
    a.jmp("make_real");

    a.here("arg_string");
    a.jsr("arg_factor");
    a.bne(mismatch);
    a.rts();

    // The flags A's kind.
    a.here("arg_factor");
    a.jsr("factor");
    a.dec(zp(z::OFFSET_A));
    a.tay();
    a.rts();

    // A `,`, else Missing ,.
    a.here("comma");
    a.cpx(imm(b','));
    a.beq(RTS);
    a.jmp("err_missing_comma");

    // Results in IAC as an integer: TRUE; A sign-extended; FALSE; A; YA.
    a.here("fn_true");
    a.lda(imm(0xFF));
    a.here("return_signed");
    a.cmp(imm(0x80));
    a.bcc("return_a");
    a.sta(zp(z::IAC));
    a.lda(imm(0xFF));
    a.sta(zp(z::IAC + 1));
    let fill = a.local();
    a.bne(fill);
    a.here("fn_false");
    a.lda(imm(0));
    a.here("return_a");
    a.ldy(imm(0));
    a.here("return_ya");
    a.sta(zp(z::IAC));
    a.sty(zp(z::IAC + 1));
    a.lda(imm(0));
    a.bind(fill);
    a.sta(zp(z::IAC + 2));
    a.sta(zp(z::IAC + 3));
    a.lda(imm(kind::INTEGER));
    a.rts();
}

fn numeric(a: &mut Asm) {
    a.here("fn_abs");
    a.jsr("arg_number");
    let real = a.local();
    a.bmi(real);
    a.lda(zp(z::IAC + 3));
    let done = a.local();
    a.bpl(done);
    a.jsr("int_negate");
    a.bind(done);
    a.lda(imm(kind::INTEGER));
    a.rts();
    // A real's kind is still in A.
    a.bind(real);
    a.ldx(imm(0));
    a.stx(zp(FAC.sign()));
    a.rts();

    // SGN: -1, 0 or 1, A 0 wherever it is 0.
    a.here("fn_sgn");
    a.jsr("arg_number");
    let (real, positive) = (a.local(), a.local());
    a.bmi(real);
    a.lda(zp(z::IAC + 3));
    a.bmi("fn_true");
    a.ora(zp(z::IAC));
    a.ora(zp(z::IAC + 1));
    a.ora(zp(z::IAC + 2));
    a.beq("return_a");
    a.bne(positive);
    a.bind(real);
    a.lda(zp(FAC.m(0)));
    a.beq("return_a");
    a.lda(zp(FAC.sign()));
    a.bmi("fn_true");
    a.bind(positive);
    a.lda(imm(1));
    a.bne("return_a");

    // INT: the next integer down.
    a.here("fn_int");
    a.jsr("arg_number");
    a.bpl(RTS);
    a.jsr("int_from_fac");
    a.lda(zp(FAC.sign()));
    let whole = a.local();
    a.bpl(whole);
    a.lda(zp(FRACTION));
    a.beq(whole);
    // Negative with a fraction: one less, which cannot pass 31 bits as the
    // truncation was below 2^31.
    a.ldx(imm(0));
    a.ldy(imm(4));
    a.clc();
    let byte = a.here_local();
    a.lda(zp(z::IAC).x());
    a.sbc(imm(0));
    a.sta(zp(z::IAC).x());
    a.inx();
    a.dey();
    a.bne(byte);
    a.bind(whole);
    a.lda(imm(kind::INTEGER));
    a.rts();

    a.here("fn_not");
    a.jsr("arg_integer");
    a.ldx(imm(3));
    let byte = a.here_local();
    a.lda(zp(z::IAC).x());
    a.eor(imm(0xFF));
    a.sta(zp(z::IAC).x());
    a.dex();
    a.bpl(byte);
    a.lda(imm(kind::INTEGER));
    a.rts();
}

fn machine(a: &mut Asm) {
    // The workspace: PAGE, TOP, LOMEM, HIMEM, COUNT, ERL and ERR.
    a.here("fn_page");
    a.lda(imm(0));
    a.ldy(zp(z::PAGE));
    a.jmp("return_ya");

    // LOMEM, HIMEM, ERL, and TO followed by P: the word at zero page X.
    let word = a.local();
    a.here("fn_lomem");
    a.ldx(imm(lo(z::LOMEM)));
    a.beq(word);
    for (routine, at) in [("fn_himem", z::HIMEM), ("fn_erl", z::ERL)] {
        a.here(routine);
        a.ldx(imm(lo(at)));
        a.bne(word);
    }
    a.here("fn_top");
    a.ldy(zp(z::OFFSET_A));
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'P'));
    let top = a.local();
    a.beq(top);
    a.jmp("err_no_such_variable");
    a.bind(top);
    a.inc(zp(z::OFFSET_A));
    a.ldx(imm(lo(z::TOP)));
    a.bind(word);
    a.lda(zp(0).x());
    a.ldy(zp(1).x());
    a.jmp("return_ya");

    a.here("fn_count");
    a.lda(zp(z::COUNT));
    a.jmp("return_a");

    a.here("fn_err");
    a.ldy(imm(0));
    a.lda(zp(ERROR_POINTER).ind_y());
    a.jmp("return_a");

    // TIME: the clock's low four bytes.
    a.here("fn_time");
    a.lda(imm(1));
    a.jsr("osword_iac");
    a.lda(imm(kind::INTEGER));
    a.rts();

    // POS and VPOS: the text cursor.
    a.here("fn_pos");
    a.jsr("cursor");
    a.txa();
    a.jmp("return_a");
}

fn devices(a: &mut Asm) {
    a.here("fn_vpos");
    a.jsr("cursor");
    a.tya();
    a.jmp("return_a");
    a.here("cursor");
    a.lda(imm(osbyte::CURSOR_POSITION));
    a.jmp(abs(entry::OSBYTE));

    // ADVAL(n): OSBYTE &80's sixteen bits.
    a.here("fn_adval");
    a.jsr("arg_integer");
    a.lda(imm(osbyte::ADVAL));
    a.ldx(zp(z::IAC));
    a.ldy(zp(z::IAC + 1));
    a.jsr(entry::OSBYTE);
    a.txa();
    a.jmp("return_ya");

    // GET: a character, Escape acknowledged and reported.
    a.here("fn_get");
    a.jsr("read_key");
    a.jmp("return_a");
    a.here("read_key");
    a.jsr(entry::OSRDCH);
    a.bcc(RTS);
    a.jmp("escaped");

    // INKEY(n): a key within n centiseconds, -1 after it; a negative n
    // asks whether that key is down.
    a.here("fn_inkey");
    a.jsr("inkey");
    let (got, signed) = (a.local(), a.local());
    a.bcc(got);
    a.bmi(signed);
    a.txa();
    a.bind(signed);
    a.jmp("return_signed");
    a.bind(got);
    a.txa();
    a.jmp("return_a");

    // OSBYTE &81 for the argument: C clear with a key in X; C set otherwise,
    // N set for a timeout, clear for a negative argument's answer in X.
    a.here("inkey");
    a.jsr("arg_integer");
    a.lda(imm(osbyte::INKEY));
    a.ldx(zp(z::IAC));
    a.ldy(zp(z::IAC + 1));
    a.jsr(entry::OSBYTE);
    a.lda(zp(z::IAC + 1));
    let scan = a.local();
    a.bmi(scan);
    a.cpy(imm(0x1B));
    let not_escape = a.local();
    a.bne(not_escape);
    a.jmp("escaped");
    a.bind(not_escape);
    a.tya();
    a.clc();
    a.beq(RTS);
    a.sec();
    a.lda(imm(0xFF));
    a.rts();
    a.bind(scan);
    a.sec();
    a.lda(imm(0));
    a.rts();

    // POINT(x,y): the colour there, or -1 off the screen.
    a.here("fn_point");
    a.jsr("expr_integer");
    a.jsr("comma");
    a.lda(zp(z::IAC));
    a.pha();
    a.lda(zp(z::IAC + 1));
    a.pha();
    a.jsr("expr_integer");
    a.jsr("closing_bracket");
    // The block at IAC: x, then y, then the colour.
    a.ldx(imm(1));
    let byte = a.here_local();
    a.lda(zp(z::IAC).x());
    a.sta(zp(z::IAC + 2).x());
    a.pla();
    a.sta(zp(z::IAC).x());
    a.dex();
    a.bpl(byte);
    a.lda(imm(9));
    a.jsr("osword_iac");
    a.lda(zp(z::IAC + 4));
    a.jmp("return_signed");

    // USR(address): A%, X% and Y%'s low bytes and C%'s low bit in, A, X, Y
    // and the flags out.
    a.here("fn_usr");
    a.jsr("arg_integer");
    a.jsr("call_iac");
    a.php();
    a.sta(zp(z::IAC));
    a.stx(zp(z::IAC + 1));
    a.sty(zp(z::IAC + 2));
    a.pla();
    a.sta(zp(z::IAC + 3));
    a.cld();
    a.lda(imm(kind::INTEGER));
    a.rts();

    // The code at IAC called with the registers from the resident integers,
    // C set from C%.
    a.here("call_iac");
    a.lda(abs(page::resident(b'C')));
    a.lsr(Acc);
    a.lda(abs(page::resident(b'A')));
    a.ldx(abs(page::resident(b'X')));
    a.ldy(abs(page::resident(b'Y')));
    a.jmp(abs(z::IAC).ind());
}

fn strings(a: &mut Asm) {
    a.here("fn_len");
    a.jsr("arg_string");
    a.lda(zp(z::STRING_LENGTH));
    a.jmp("return_a");

    // ASC: the first character, -1 for none.
    a.here("fn_asc");
    a.jsr("arg_string");
    a.lda(zp(z::STRING_LENGTH));
    let some = a.local();
    a.bne(some);
    a.jmp("fn_true");
    a.bind(some);
    a.lda(abs(page::STRING));
    a.jmp("return_a");

    // GET$, INKEY$ and CHR$: one character, or INKEY$'s none, A the kind.
    a.here("fn_get_string");
    a.jsr("read_key");
    a.bcc("one_character");
    a.here("fn_inkey_string");
    a.jsr("inkey");
    a.txa();
    a.bcc("one_character");
    a.lda(imm(0));
    a.sta(zp(z::STRING_LENGTH));
    a.rts();
    a.here("fn_chr");
    a.jsr("arg_integer");
    a.lda(zp(z::IAC));
    a.here("one_character");
    a.sta(abs(page::STRING));
    a.lda(imm(1));
    a.sta(zp(z::STRING_LENGTH));
    a.lda(imm(kind::STRING));
    a.rts();

    // STR$ and STR$~.
    a.here("fn_str");
    a.ldy(zp(z::OFFSET_A));
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'~'));
    let decimal = a.local();
    a.bne(decimal);
    a.inc(zp(z::OFFSET_A));
    a.jsr("arg_integer");
    a.jmp("hex_string");
    a.bind(decimal);
    a.jsr("arg_real");
    a.jsr("format_str");
    a.lda(imm(kind::STRING));
    a.rts();

    // VAL: the number at the string's start after spaces and a sign, read
    // as from a program.
    a.here("fn_val");
    a.jsr("arg_string");
    // The string buffer's number, as VAL reads it.
    a.here("val_buffer");
    a.lda(zp(z::OFFSET_A));
    a.pha();
    a.lda(zp(z::TEXT_A));
    a.pha();
    a.lda(zp(z::TEXT_A + 1));
    a.pha();
    a.jsr("string_cr");
    a.stx(zp(z::TEXT_A));
    a.sty(zp(z::TEXT_A + 1));
    a.ldy(imm(0));
    let spaces = a.here_local();
    a.lda(zp(z::TEXT_A).ind_y());
    a.iny();
    a.cmp(imm(b' '));
    a.beq(spaces);
    a.ldx(imm(0));
    a.cmp(imm(b'-'));
    let (unsigned, signed) = (a.local(), a.local());
    a.bne(unsigned);
    a.dex();
    a.bne(signed);
    a.bind(unsigned);
    a.cmp(imm(b'+'));
    a.beq(signed);
    a.dey();
    a.bind(signed);
    a.txa();
    a.pha();
    a.jsr("read_number");
    a.sta(zp(z::TYPE));
    a.pla();
    let (positive, real) = (a.local(), a.local());
    a.beq(positive);
    a.bit(zp(z::TYPE));
    a.bmi(real);
    a.jsr("int_negate");
    a.jmp(positive);
    a.bind(real);
    a.jsr("fac_negate");
    a.bind(positive);
    a.pla();
    a.sta(zp(z::TEXT_A + 1));
    a.pla();
    a.sta(zp(z::TEXT_A));
    a.pla();
    a.sta(zp(z::OFFSET_A));
    a.lda(zp(z::TYPE));
    a.rts();
}

fn slicing(a: &mut Asm) {
    // LEFT$(s, n), MID$(s, m[, n]) and RIGHT$(s, n): the string waits on
    // the stack while the numbers are found.
    a.here("fn_left");
    a.jsr("string_and_count");
    a.lda(imm(0));
    a.beq("take");

    a.here("fn_mid");
    a.jsr("string_and_count");
    a.lda(zp(z::IAC));
    a.pha();
    a.lda(imm(0xFF));
    a.sta(zp(z::IAC));
    a.cpx(imm(b','));
    let counted = a.local();
    a.bne(counted);
    a.jsr("expr_integer");
    a.bind(counted);
    a.pla();
    a.beq("take");
    a.sec();
    a.sbc(imm(1));
    a.bcs("take");

    // The bracket's string pushed, then its count after a comma: X the
    // character after the count.
    a.here("string_and_count");
    a.jsr("expr_string");
    a.jsr("comma");
    a.jsr("push_value");
    a.jmp("expr_integer");

    a.here("fn_right");
    a.jsr("string_and_count");
    a.ldy(imm(0));
    a.lda(zp(z::STACK).ind_y());
    a.sec();
    a.sbc(zp(z::IAC));
    a.bcs("take");
    a.lda(imm(0));
    // IAC characters of the stacked string from offset A, as many as there
    // are, counts and offsets their low bytes; then the string dropped.
    a.here("take");
    a.sta(zp(FROM_OFFSET));
    a.jsr("closing_bracket");
    a.ldy(imm(0));
    a.lda(zp(z::STACK).ind_y());
    a.sec();
    a.sbc(zp(FROM_OFFSET));
    let (some, fewer) = (a.local(), a.local());
    a.bcs(some);
    a.lda(imm(0));
    a.bind(some);
    a.cmp(zp(z::IAC));
    a.bcc(fewer);
    a.lda(zp(z::IAC));
    a.bind(fewer);
    a.sta(zp(z::STRING_LENGTH));
    a.ldx(imm(0));
    a.ldy(zp(FROM_OFFSET));
    let (copy, copied) = (a.here_local(), a.local());
    a.cpx(zp(z::STRING_LENGTH));
    a.beq(copied);
    a.iny();
    a.lda(zp(z::STACK).ind_y());
    a.sta(abs(page::STRING).x());
    a.inx();
    a.bne(copy);
    a.bind(copied);
    a.jsr("pop_string");
    a.lda(imm(kind::STRING));
    a.rts();

    // STRING$(n, s): s n times, n its low byte.
    a.here("fn_string");
    a.jsr("expr_integer");
    a.jsr("comma");
    a.lda(zp(z::IAC));
    a.pha();
    a.jsr("expr_string");
    a.jsr("closing_bracket");
    // STRING_LENGTH stays the copied string's until all are made.
    a.pla();
    a.sta(zp(FROM_OFFSET));
    let (repeat, done, empty) = (a.local(), a.local(), a.local());
    a.beq(empty);
    a.ldx(zp(z::STRING_LENGTH));
    a.bind(repeat);
    a.dec(zp(FROM_OFFSET));
    a.beq(done);
    a.ldy(imm(0));
    let character = a.here_local();
    a.cpy(zp(z::STRING_LENGTH));
    a.beq(repeat);
    a.lda(abs(page::STRING).y());
    a.sta(abs(page::STRING).x());
    a.iny();
    a.inx();
    a.bne(character);
    a.jmp("err_string_too_long");
    a.bind(empty);
    a.tax();
    a.bind(done);
    a.stx(zp(z::STRING_LENGTH));
    a.lda(imm(kind::STRING));
    a.rts();
}

fn searching(a: &mut Asm) {
    // INSTR(a, b[, n]): where b first appears in a from position n, 1 by
    // default, 0 if nowhere.
    a.here("fn_instr");
    a.jsr("expr_string");
    a.jsr("comma");
    a.jsr("push_value");
    a.jsr("expr_string");
    a.ldy(imm(1));
    a.sty(zp(z::IAC));
    a.cpx(imm(b','));
    let started = a.local();
    a.bne(started);
    a.jsr("push_value");
    a.jsr("expr_integer");
    a.jsr("closing_bracket");
    // The sought string, stacked above the searched one, to the buffer.
    a.ldy(imm(0));
    a.lda(zp(z::STACK).ind_y());
    a.sta(zp(z::STRING_LENGTH));
    a.tay();
    let popped = a.local();
    a.beq(popped);
    let copy = a.here_local();
    a.lda(zp(z::STACK).ind_y());
    a.sta(abs(page::STRING - 1).y());
    a.dey();
    a.bne(copy);
    a.bind(popped);
    a.jsr("pop_string");
    a.jmp("search");
    a.bind(started);
    a.jsr("closing_bracket");
    // The sought string is in the buffer, the string searched stacked.
    a.here("search");
    a.lda(zp(z::IAC));
    let positive = a.local();
    a.bne(positive);
    a.lda(imm(1));
    a.bind(positive);
    a.sta(zp(FROM_OFFSET));
    a.ldy(imm(0));
    a.lda(zp(z::STACK).ind_y());
    a.sta(zp(super::vars::LENGTH));
    // Each start from n while the sought string still fits.
    let (try_, next, found, missing) = (a.here_local(), a.local(), a.local(), a.local());
    a.lda(zp(FROM_OFFSET));
    a.clc();
    a.adc(zp(z::STRING_LENGTH));
    a.bcs(missing);
    a.sbc(imm(0));
    a.cmp(zp(super::vars::LENGTH));
    a.beq(next);
    a.bcs(missing);
    a.bind(next);
    a.ldx(imm(0));
    a.ldy(zp(FROM_OFFSET));
    let character = a.here_local();
    a.cpx(zp(z::STRING_LENGTH));
    a.beq(found);
    a.lda(zp(z::STACK).ind_y());
    a.cmp(abs(page::STRING).x());
    let differs = a.local();
    a.bne(differs);
    a.iny();
    a.inx();
    a.bne(character);
    a.bind(differs);
    a.inc(zp(FROM_OFFSET));
    a.bne(try_);
    a.bind(missing);
    a.lda(imm(0));
    a.sta(zp(FROM_OFFSET));
    a.bind(found);
    a.jsr("pop_string");
    a.lda(zp(FROM_OFFSET));
    a.jmp("return_a");
}

fn evaluating(a: &mut Asm) {
    // EVAL: the string tokenised as an expression and evaluated from a copy
    // on the stack.
    a.here("fn_eval");
    a.jsr("arg_string");
    a.jsr("string_cr");
    a.txa();
    a.ldx(imm(hi(page::STRING)));
    a.jsr("crunch_expression");
    a.stx(zp(z::STRING_LENGTH));
    a.inc(zp(z::STRING_LENGTH));
    a.lda(imm(kind::STRING));
    a.jsr("push_value");
    a.lda(zp(z::OFFSET_A));
    a.pha();
    a.lda(zp(z::TEXT_A));
    a.pha();
    a.lda(zp(z::TEXT_A + 1));
    a.pha();
    a.lda(zp(z::STACK));
    a.clc();
    a.adc(imm(1));
    a.sta(zp(z::TEXT_A));
    a.lda(zp(z::STACK + 1));
    a.adc(imm(0));
    a.sta(zp(z::TEXT_A + 1));
    a.lda(imm(0));
    a.sta(zp(z::OFFSET_A));
    a.jsr("expr");
    a.tay();
    a.pla();
    a.sta(zp(z::TEXT_A + 1));
    a.pla();
    a.sta(zp(z::TEXT_A));
    a.pla();
    a.sta(zp(z::OFFSET_A));
    a.tya();
    a.pha();
    a.jsr("pop_string");
    a.pla();
    a.rts();
}

/// A CR after the string buffer's text, then XY the buffer.
fn string_cr(a: &mut Asm) {
    a.here("string_cr");
    a.ldy(zp(z::STRING_LENGTH));
    a.lda(imm(CR));
    a.sta(abs(page::STRING).y());
    a.ldx(imm(lo(page::STRING)));
    a.ldy(imm(hi(page::STRING)));
    a.rts();
}

fn table(a: &mut Asm) {
    a.here("function_low");
    for routine in FUNCTIONS {
        a.byte(lo(sym(routine) + -1));
    }
    a.here("function_high");
    for routine in FUNCTIONS {
        a.byte(hi(sym(routine) + -1));
    }
}
