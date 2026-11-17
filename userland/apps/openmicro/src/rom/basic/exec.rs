//! Running: each statement in turn, from a typed line or the program, its
//! keyword's handler chosen by token.

use tairix_openbasic::basic2;
use tairix_openbasic::keyword::{word, Word, PSEUDO_LHS};

use super::ws::{page, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Acc, Asm, RTS};
use crate::rom::os::{entry, CR, ESCAPE_FLAG};

/// The token of the first statement, the first command too.
pub(super) const FIRST_STATEMENT: u8 = basic2::AUTO.token;

/// The commands, AUTO to SAVE, in token order from the first: as BASIC II
/// has them, only a typed line's first keyword, and a Syntax error anywhere
/// else.
pub(super) const COMMANDS: [(Word<u8>, &str); 8] = [
    (basic2::AUTO, "statement_auto"),
    (basic2::DELETE, "statement_delete"),
    (basic2::LOAD, "statement_load"),
    (basic2::LIST, "statement_list"),
    (basic2::NEW, "statement_new"),
    (basic2::OLD, "statement_old"),
    (basic2::RENUMBER, "statement_renumber"),
    (basic2::SAVE, "statement_save"),
];
const _: () = assert!(super::in_token_order(FIRST_STATEMENT, &COMMANDS));

/// The statements, by token.
const HANDLED: [(u8, &str); 49] = [
    (word::PTR.token | PSEUDO_LHS, "statement_ptr"),
    (word::PAGE.token | PSEUDO_LHS, "statement_page"),
    (word::TIME.token | PSEUDO_LHS, "statement_time"),
    (word::LOMEM.token | PSEUDO_LHS, "statement_lomem"),
    (word::HIMEM.token | PSEUDO_LHS, "statement_himem"),
    (word::SOUND.token, "statement_sound"),
    (word::BPUT.token, "statement_bput"),
    (word::CALL.token, "statement_call"),
    (word::CHAIN.token, "statement_chain"),
    (word::CLEAR.token, "statement_clear"),
    (word::CLOSE.token, "statement_close"),
    (word::CLG.token, "statement_clg"),
    (word::CLS.token, "statement_cls"),
    (word::DATA.token, "skip_line"),
    (word::DEF.token, "skip_line"),
    (word::DIM.token, "statement_dim"),
    (word::DRAW.token, "statement_draw"),
    (word::END.token, "prompt_checked"),
    (word::ENDPROC.token, "statement_endproc"),
    (word::ENVELOPE.token, "statement_envelope"),
    (word::FOR.token, "statement_for"),
    (word::GOSUB.token, "statement_gosub"),
    (word::GOTO.token, "statement_goto"),
    (word::GCOL.token, "statement_gcol"),
    (word::IF.token, "statement_if"),
    (word::INPUT.token, "statement_input"),
    (word::LET.token, "statement_let"),
    (word::LOCAL.token, "statement_local"),
    (word::MODE.token, "statement_mode"),
    (word::MOVE.token, "statement_move"),
    (word::NEXT.token, "statement_next"),
    (word::ON.token, "statement_on"),
    (word::VDU.token, "statement_vdu"),
    (word::PLOT.token, "statement_plot"),
    (word::PRINT.token, "statement_print"),
    (word::PROC.token, "statement_proc"),
    (word::READ.token, "statement_read"),
    (word::REM.token, "skip_line"),
    (word::REPEAT.token, "statement_repeat"),
    (word::REPORT.token, "statement_report"),
    (word::RESTORE.token, "statement_restore"),
    (word::RETURN.token, "statement_return"),
    (word::RUN.token, "statement_run"),
    (word::STOP.token, "err_stop"),
    (word::COLOUR.token, "statement_colour"),
    (word::TRACE.token, "statement_trace"),
    (word::UNTIL.token, "statement_until"),
    (word::WIDTH.token, "statement_width"),
    (word::OSCLI.token, "statement_oscli"),
];

/// Each statement token's handler from &C6, a command's a Syntax error; any
/// other token there is a Mistake.
const STATEMENTS: [&str; 0x100 - FIRST_STATEMENT as usize] = {
    let mut handlers = ["err_mistake"; 0x100 - FIRST_STATEMENT as usize];
    let mut index = 0;
    while index < COMMANDS.len() {
        handlers[index] = "statement_syntax";
        index += 1;
    }
    let mut index = 0;
    while index < HANDLED.len() {
        let (token, handler) = HANDLED[index];
        handlers[(token - FIRST_STATEMENT) as usize] = handler;
        index += 1;
    }
    handlers
};

/// Page 4, where the resident integers are, is also an integer location's
/// type: one load sets both.
const RESIDENT_PAGE: u8 = super::ws::place::INTEGER;
const _: () = assert!(page::RESIDENT >> 8 == RESIDENT_PAGE as u16);

pub fn emit(a: &mut Asm) {
    looping(a);
    running(a);
    statements(a);
    tables(a);
}

fn looping(a: &mut Asm) {
    // ON ERROR OFF: the report again.
    let off = a.local();
    a.bind(off);
    a.lda(imm(0));
    a.sta(zp(z::ON_ERROR + 1));
    a.beq("statement_done");

    // The CR just read starts the next record.
    a.here("next_text_record");
    a.ldy(zp(z::OFFSET_A));
    a.dey();
    a.tya();
    a.clc();
    a.adc(zp(z::TEXT_A));
    a.sta(zp(z::TEXT_A));
    a.bcc(RTS);
    a.inc(zp(z::TEXT_A + 1));
    a.rts();

    // The statement's end read again from the text, just behind the
    // statement pointer: nothing the statement ran, a program's code or a
    // store into BASIC's scratch, can lose it, as BASIC II's cannot.
    a.here("statement_ended");
    a.ldy(zp(z::OFFSET_A));
    a.dey();
    a.lda(zp(z::TEXT_A).ind_y());
    a.tax();
    a.jmp("statement_done_x");

    a.here("statement_report");
    a.jsr("report");
    a.fall_into("statement_done");

    // What may follow a statement: another, the line's end, or an ELSE the
    // statement before it has passed.
    a.here("statement_done");
    a.ldy(zp(z::OFFSET_A));
    let space = a.here_local();
    a.lda(zp(z::TEXT_A).ind_y());
    a.iny();
    a.cmp(imm(b' '));
    a.beq(space);
    a.sty(zp(z::OFFSET_A));
    a.tax();
    a.here("statement_done_x");
    a.cpx(imm(b':'));
    a.beq("statement");
    a.cpx(imm(CR));
    a.beq("end_of_line");
    a.cpx(imm(word::ELSE.token));
    a.beq("skip_line");
    a.here("statement_syntax");
    a.jmp("err_syntax");

    // ON ERROR statements, kept for an error to run.
    a.here("statement_on_error");
    a.jsr("next_a");
    a.cmp(imm(word::OFF.token));
    a.beq(off);
    a.dec(zp(z::OFFSET_A));
    a.jsr("return_point");
    a.stx(zp(z::ON_ERROR));
    a.sty(zp(z::ON_ERROR + 1));
    a.fall_into("skip_line");

    // The rest of the line passed over.
    a.here("skip_line");
    a.ldy(zp(z::OFFSET_A));
    let byte = a.here_local();
    a.lda(zp(z::TEXT_A).ind_y());
    a.iny();
    a.cmp(imm(CR));
    a.bne(byte);
    a.sty(zp(z::OFFSET_A));

    // A typed line ends at the prompt; a program's goes on to its next
    // record, unless that is the end or Escape is pending.
    a.here("end_of_line");
    a.lda(zp(z::TEXT_A + 1));
    a.cmp(imm(hi(page::INPUT)));
    let program = a.local();
    a.bne(program);
    a.here("end_run");
    a.jmp("prompt");
    a.bind(program);
    a.jsr("next_text_record");
}

fn running(a: &mut Asm) {
    a.here("run_record");
    a.ldy(imm(1));
    a.lda(zp(z::TEXT_A).ind_y());
    a.bmi("end_run");
    a.lda(zp(z::TRACE));
    let untraced = a.local();
    a.beq(untraced);
    a.jsr("trace_line");
    a.bind(untraced);
    a.lda(imm(4));
    a.sta(zp(z::OFFSET_A));
    a.fall_into("statement");

    // The statement at the statement pointer, its first character read as
    // `next_a` reads it.
    a.here("statement");
    a.bit(zp(ESCAPE_FLAG));
    let escaped = a.local();
    a.bmi(escaped);
    a.ldy(zp(z::OFFSET_A));
    let space = a.here_local();
    a.lda(zp(z::TEXT_A).ind_y());
    a.iny();
    a.cmp(imm(b' '));
    a.beq(space);
    a.sty(zp(z::OFFSET_A));
    a.cmp(imm(FIRST_STATEMENT));
    let other = a.local();
    a.bcc(other);
    a.sbc(imm(FIRST_STATEMENT));
    a.tax();
    a.lda(abs("statement_high").x());
    a.pha();
    a.lda(abs("statement_low").x());
    a.pha();
    a.rts();
    a.bind(escaped);
    a.jmp("escaped");
    // An assignment to a name, the commonest, looked for first.
    a.bind(other);
    let (low, high, assigning, assigned) = (a.local(), a.local(), a.local(), a.local());
    a.cmp(imm(b'A'));
    a.bcc(low);
    a.cmp(imm(b'Z' + 1));
    a.bcs(high);
    // A letter always starts a location.
    a.jsr("lvalue_letter");
    a.bcc(assigned);
    a.bind(high);
    // A true IF whose statements are none ends where its ELSE part begins.
    a.cmp(imm(word::ELSE.token));
    a.beq("skip_line");
    a.cmp(imm(b'['));
    a.bne(assigning);
    a.jmp("statement_assemble");
    a.bind(low);
    a.cmp(imm(b':'));
    a.beq("statement");
    a.cmp(imm(CR));
    a.beq("end_of_line");
    a.cmp(imm(b'*'));
    a.beq("statement_star");
    a.cmp(imm(b'='));
    a.bne(assigning);
    a.jmp("statement_return_value");
    a.bind(assigning);
    a.dec(zp(z::OFFSET_A));
    a.fall_into("assignment");

    // LET and a bare assignment: the location, then `=` and the value.
    a.here("assignment");
    a.here("statement_let");
    a.jsr("lvalue");
    let none = a.local();
    a.bcs(none);
    a.bind(assigned);
    a.lda(zp(z::VAR));
    a.pha();
    a.lda(zp(z::VAR + 1));
    a.pha();
    a.lda(zp(z::VTYPE));
    a.pha();
    a.jsr("next_a");
    a.cmp(imm(b'='));
    let mistake = a.local();
    a.bne(mistake);
    a.jsr("expr");
    a.tay();
    a.pla();
    a.sta(zp(z::VTYPE));
    a.pla();
    a.sta(zp(z::VAR + 1));
    a.pla();
    a.sta(zp(z::VAR));
    a.tya();
    a.jsr("store_variable");
    a.jmp("statement_ended");
    a.bind(mistake);
    a.jmp("err_mistake");
    a.bind(none);
    a.jmp("err_syntax");
}

fn statements(a: &mut Asm) {
    // *command: the rest of the line, from the star, to OSCLI.
    a.here("statement_star");
    a.jsr("return_point");
    a.txa();
    let same = a.local();
    a.bne(same);
    a.dey();
    a.bind(same);
    a.dex();
    a.jsr(entry::OSCLI);
    a.jmp("skip_line");

    // RUN: the variables cleared, then the program from its first record.
    // A program that does not start with a CR is a Syntax error before any
    // line, as BASIC II finds its first line's end there.
    a.here("statement_run");
    a.jsr("clear_variables");
    a.ldx(imm(lo(z::TEXT_A)));
    a.jsr("page_pointer");
    a.tay();
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(CR));
    let unstarted = a.local();
    a.bne(unstarted);
    a.jmp("run_record");
    a.bind(unstarted);
    a.lda(imm(hi(page::INPUT)));
    a.sta(zp(z::TEXT_A + 1));
    a.jmp("err_syntax");
    lvalues(a);
}

/// A location at the statement pointer, made if a variable is absent: ?, !
/// or $ and a factor, or a variable and any ? or ! after it. C set if there
/// is none; `@` alone is none.
fn lvalues(a: &mut Asm) {
    a.here("lvalue");
    a.jsr("next_a");
    let (low, high, none, named, lowercase, found, after_name, indirect, binary) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.cmp(imm(b'A'));
    a.bcc(low);
    a.cmp(imm(b'Z' + 1));
    a.bcs(high);
    // A name from its initial, @ to Z, the pointer past it: a resident
    // integer found at once.
    a.here("lvalue_letter");
    a.asl(Acc);
    a.asl(Acc);
    a.tax();
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'%'));
    a.bne(named);
    a.iny();
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'('));
    a.beq(named);
    a.sty(zp(z::OFFSET_A));
    a.stx(zp(z::VAR));
    a.ldx(imm(RESIDENT_PAGE));
    a.stx(zp(z::VAR + 1));
    a.stx(zp(z::VTYPE));
    a.jmp(after_name);
    a.bind(high);
    a.jsr("is_name");
    a.bcs(lowercase);
    a.bind(low);
    a.ldx(imm(super::ws::place::BYTE));
    a.cmp(imm(b'?'));
    a.beq(indirect);
    a.ldx(imm(super::ws::place::INTEGER));
    a.cmp(imm(b'!'));
    a.beq(indirect);
    a.ldx(imm(super::ws::place::FIXED_STRING));
    a.cmp(imm(b'$'));
    a.beq(indirect);
    a.cmp(imm(b'@'));
    a.beq("lvalue_letter");
    a.bind(none);
    a.sec();
    a.rts();
    a.bind(indirect);
    a.txa();
    a.pha();
    a.jsr("factor_integer");
    a.pla();
    a.here("lvalue_at_iac");
    a.dec(zp(z::OFFSET_A));
    a.sta(zp(z::VTYPE));
    a.lda(zp(z::IAC));
    a.sta(zp(z::VAR));
    a.lda(zp(z::IAC + 1));
    a.sta(zp(z::VAR + 1));
    a.clc();
    a.rts();
    // X the initial's resident offset, none for `@`.
    a.bind(named);
    a.txa();
    a.beq(none);
    a.bind(lowercase);
    a.dec(zp(z::OFFSET_A));
    a.jsr("variable_named");
    a.bcc(found);
    a.jsr("create_variable");
    a.bind(found);
    a.ldy(zp(z::OFFSET_A));
    a.lda(zp(z::TEXT_A).ind_y());
    a.bind(after_name);
    a.ldx(imm(super::ws::place::BYTE));
    a.cmp(imm(b'?'));
    a.beq(binary);
    a.ldx(imm(super::ws::place::INTEGER));
    a.cmp(imm(b'!'));
    a.beq(binary);
    a.clc();
    a.rts();
    a.bind(binary);
    a.inc(zp(z::OFFSET_A));
    a.txa();
    a.pha();
    a.jsr("load_variable");
    a.jsr("plus_offset");
    a.pla();
    a.jmp("lvalue_at_iac");

    // A location at the statement pointer; Mistake if there is none.
    a.here("location");
    a.jsr("lvalue");
    a.bcc(RTS);
    a.jmp("err_mistake");
}

/// Each handler's address less one, as RTS takes it.
fn tables(a: &mut Asm) {
    a.here("statement_low");
    for handler in STATEMENTS {
        a.byte(lo(sym(handler) + -1));
    }
    a.here("statement_high");
    for handler in STATEMENTS {
        a.byte(hi(sym(handler) + -1));
    }
    a.here("command_low");
    for (_, handler) in COMMANDS {
        a.byte(lo(sym(handler) + -1));
    }
    a.here("command_high");
    for (_, handler) in COMMANDS {
        a.byte(hi(sym(handler) + -1));
    }
}
