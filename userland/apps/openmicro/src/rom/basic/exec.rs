//! Running: each statement in turn, from a typed line or the program, its
//! keyword's handler chosen by token.

use super::ws::{page, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Asm, RTS};
use crate::rom::os::{entry, ESCAPE_FLAG};

/// The first statement token.
const FIRST_STATEMENT: u8 = 0xC6;
const ELSE: u8 = 0x8B;
const CR: u8 = 0x0D;

/// The commands, AUTO to SAVE, by token from the first: as BASIC II has
/// them, only a typed line's first keyword, and a Syntax error anywhere
/// else.
pub(super) const COMMANDS: [&str; 8] = [
    "statement_auto",
    "statement_delete",
    "statement_load",
    "statement_list",
    "statement_new",
    "statement_old",
    "statement_renumber",
    "statement_save",
];

/// The statements, by token.
const HANDLED: [(u8, &str); 49] = [
    (0xCF, "statement_ptr"),
    (0xD0, "statement_page"),
    (0xD1, "statement_time"),
    (0xD2, "statement_lomem"),
    (0xD3, "statement_himem"),
    (0xD4, "statement_sound"),
    (0xD5, "statement_bput"),
    (0xD6, "statement_call"),
    (0xD7, "statement_chain"),
    (0xD8, "statement_clear"),
    (0xD9, "statement_close"),
    (0xDA, "statement_clg"),
    (0xDB, "statement_cls"),
    (0xDC, "skip_line"),
    (0xDD, "skip_line"),
    (0xDE, "statement_dim"),
    (0xDF, "statement_draw"),
    (0xE0, "prompt_checked"),
    (0xE1, "statement_endproc"),
    (0xE2, "statement_envelope"),
    (0xE3, "statement_for"),
    (0xE4, "statement_gosub"),
    (0xE5, "statement_goto"),
    (0xE6, "statement_gcol"),
    (0xE7, "statement_if"),
    (0xE8, "statement_input"),
    (0xE9, "statement_let"),
    (0xEA, "statement_local"),
    (0xEB, "statement_mode"),
    (0xEC, "statement_move"),
    (0xED, "statement_next"),
    (0xEE, "statement_on"),
    (0xEF, "statement_vdu"),
    (0xF0, "statement_plot"),
    (0xF1, "statement_print"),
    (0xF2, "statement_proc"),
    (0xF3, "statement_read"),
    (0xF4, "skip_line"),
    (0xF5, "statement_repeat"),
    (0xF6, "statement_report"),
    (0xF7, "statement_restore"),
    (0xF8, "statement_return"),
    (0xF9, "statement_run"),
    (0xFA, "err_stop"),
    (0xFB, "statement_colour"),
    (0xFC, "statement_trace"),
    (0xFD, "statement_until"),
    (0xFE, "statement_width"),
    (0xFF, "statement_oscli"),
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

    a.here("statement_report");
    a.jsr("report");
    a.fall_into("statement_done");

    // What may follow a statement: another, the line's end, or an ELSE the
    // statement before it has passed.
    a.here("statement_done");
    a.jsr("next_a");
    a.tax();
    a.here("statement_done_x");
    a.cpx(imm(b':'));
    a.beq("statement");
    a.cpx(imm(CR));
    a.beq("end_of_line");
    a.cpx(imm(ELSE));
    a.beq("skip_line");
    a.here("statement_syntax");
    a.jmp("err_syntax");

    // ON ERROR statements, kept for an error to run.
    a.here("statement_on_error");
    a.jsr("next_a");
    a.cmp(imm(0x87));
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

    // The statement at the statement pointer.
    a.here("statement");
    a.bit(zp(ESCAPE_FLAG));
    let going = a.local();
    a.bpl(going);
    a.jmp("escaped");
    a.bind(going);
    a.jsr("next_a");
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
    a.bind(other);
    a.cmp(imm(b':'));
    a.beq("statement");
    a.cmp(imm(CR));
    a.beq("end_of_line");
    // A true IF whose statements are none ends where its ELSE part begins.
    a.cmp(imm(ELSE));
    a.beq("skip_line");
    a.cmp(imm(b'*'));
    a.beq("statement_star");
    a.cmp(imm(b'['));
    let not_assembly = a.local();
    a.bne(not_assembly);
    a.jmp("statement_assemble");
    a.bind(not_assembly);
    a.cmp(imm(b'='));
    let assigning = a.local();
    a.bne(assigning);
    a.jmp("statement_return_value");
    a.bind(assigning);
    a.dec(zp(z::OFFSET_A));
    a.fall_into("assignment");

    // LET and a bare assignment: the location, then `=` and the value.
    a.here("assignment");
    a.here("statement_let");
    a.jsr("lvalue");
    a.bcs("statement_syntax");
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
    a.stx(zp(z::OP));
    a.tay();
    a.pla();
    a.sta(zp(z::VTYPE));
    a.pla();
    a.sta(zp(z::VAR + 1));
    a.pla();
    a.sta(zp(z::VAR));
    a.tya();
    a.jsr("store_variable");
    a.ldx(zp(z::OP));
    a.jmp("statement_done_x");
    a.bind(mistake);
    a.jmp("err_mistake");

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
}

fn statements(a: &mut Asm) {
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

    // A location at the statement pointer, made if a variable is absent:
    // ?, ! or $ and a factor, or a variable and any ? or ! after it. C set
    // if there is none; `@` alone is none.
    a.here("lvalue");
    a.jsr("next_a");
    let (indirect, name, none, at) = (a.local(), a.local(), a.local(), a.local());
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
    a.beq(at);
    a.cmp(imm(b'9' + 1));
    a.bcc(none);
    a.jsr("is_name");
    a.bcs(name);
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
    a.bind(at);
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'%'));
    a.bne(none);
    a.bind(name);
    a.dec(zp(z::OFFSET_A));
    a.jsr("variable");
    let found = a.local();
    a.bcc(found);
    a.jsr("create_variable");
    a.bind(found);
    a.ldy(zp(z::OFFSET_A));
    a.lda(zp(z::TEXT_A).ind_y());
    a.ldx(imm(super::ws::place::BYTE));
    a.cmp(imm(b'?'));
    let binary = a.local();
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
    a.jsr("make_integer");
    a.jsr("push_value");
    a.jsr("factor_integer");
    a.jsr("pop_int");
    a.jsr("int_add");
    a.pla();
    a.jmp("lvalue_at_iac");
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
    for handler in COMMANDS {
        a.byte(lo(sym(handler) + -1));
    }
    a.here("command_high");
    for handler in COMMANDS {
        a.byte(hi(sym(handler) + -1));
    }
}

/// The token of the first command.
pub(super) const FIRST_COMMAND: u8 = FIRST_STATEMENT;
