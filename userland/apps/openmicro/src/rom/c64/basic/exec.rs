//! Running the program: the statement loop, the STOP key, and the
//! statements that steer it — END, STOP, CONT, RUN, CLR, NEW, GOTO,
//! GOSUB, RETURN, IF, ON, REM, DATA, FOR and NEXT — with POKE, WAIT and
//! SYS.

use super::errors::{number, raise};
use super::text::{LINE_FOUND, STACK_BASE};
use super::tokens::token;
use super::ws::{page, vector, zp};
use crate::cpu::STACK_PAGE;
use crate::rom::asm::{abs, hi, imm, lo, sym, zp as z, Asm, RTS};
use crate::rom::c64::kernal::entry as kernal;

/// A GOSUB's frame on the stack: its token, CURLIN and TXTPTR.
const GOSUB_FRAME: u8 = 5;
/// A FOR's frame: its token, the variable's address, the step packed and
/// its sign, the limit packed, CURLIN and TXTPTR.
const FOR_FRAME: u8 = 18;
/// Where each part of a frame lies from page 1 indexed by the stack
/// pointer at its token.
mod frame {
    use crate::cpu::STACK_PAGE;

    pub const TOKEN: u16 = STACK_PAGE + 1;
    pub const VARIABLE: u16 = STACK_PAGE + 2;
    pub const STEP: u8 = 4;
    pub const STEP_SIGN: u16 = STACK_PAGE + 9;
    pub const LIMIT: u8 = 0x0A;
    pub const LINE: u16 = STACK_PAGE + 0x0F;
    pub const TEXT: u16 = STACK_PAGE + 0x11;
}
/// The stack left free when a frame is pushed; less is OUT OF MEMORY.
const STACK_RESERVE: u8 = 0x3E;
/// ON's statement and the targets left to pass.
const ON_STATEMENT: u16 = zp::OPMASK;
const ON_COUNT: u16 = zp::COUNT;
/// WAIT's mask and the bits it inverts.
const WAIT_MASK: u16 = zp::CHARAC;
const WAIT_INVERT: u16 = zp::ENDCHR;

pub fn emit(a: &mut Asm) {
    a.movable(statement_loop);
    a.movable(ends);
    a.movable(run);
    a.movable(jumps);
    a.movable(conditions);
    a.movable(skips);
    a.movable(frames);
    a.movable(for_loop);
    a.movable(next_loop);
    a.movable(memory);
}

/// `gone`, IGONE's default: the next statement fetched and run.
pub(super) fn gone(a: &mut Asm) {
    a.here("gone");
    a.jsr(zp::CHRGET);
    a.jsr("execute");
    a.jmp("next_statement");
}

/// `next_statement`: after a statement, STOP checked and, in a program, the
/// place kept for CONT; then the next statement after a colon, or the next
/// line, CURLIN its number; past the last line, READY. `run_direct`: the
/// tokenised buffer run.
fn statement_loop(a: &mut Asm) {
    a.here("run_direct");
    a.jsr(zp::CHRGET);
    a.jsr("execute");
    a.fall_into("next_statement");

    a.here("next_statement");
    let keep = a.local();
    let kept = a.local();
    let line_end = a.local();
    let more = a.local();
    let going = a.local();
    a.jsr(kernal::STOP);
    a.bne(going);
    a.jmp("break");
    a.bind(going);
    a.ldy(imm(0));
    a.lda(z(zp::TXTPTR).ind_y());
    a.beq(line_end);
    a.cmp(imm(b':'));
    a.beq(keep);
    a.jmp("syntax_error");
    a.bind(line_end);
    a.ldy(imm(2));
    a.lda(z(zp::TXTPTR).ind_y());
    a.bne(more);
    a.jmp("ready");
    a.bind(more);
    a.iny();
    a.lda(z(zp::TXTPTR).ind_y());
    a.sta(z(zp::CURLIN));
    a.iny();
    a.lda(z(zp::TXTPTR).ind_y());
    a.sta(z(zp::CURLIN + 1));
    // The boundary kept for CONT and INPUT's redo, the line's mode now
    // known; Y what TXTPTR steps past to the statement.
    a.bind(keep);
    a.ldx(z(zp::CURLIN + 1));
    a.inx();
    a.beq(kept);
    a.lda(z(zp::TXTPTR));
    a.sta(z(zp::OLDTXT));
    a.lda(z(zp::TXTPTR + 1));
    a.sta(z(zp::OLDTXT + 1));
    a.bind(kept);
    a.tya();
    a.beq("gone_vector");
    a.jsr("add_txtptr");
    a.here("gone_vector");
    a.jmp(abs(vector::IGONE).ind());

    // The statement whose first character is A: a token's routine, or LET
    // for a name; GO TO is GOTO.
    a.here("execute");
    let token_statement = a.local();
    let syntax = a.local();
    a.beq(RTS);
    // CHRGET's flags do not show the character's top bit.
    a.tax();
    a.bmi(token_statement);
    a.jmp("st_let");
    a.bind(token_statement);
    a.cpx(imm(token::NEW + 1));
    a.bcs(syntax);
    let base = -i32::from(token::END);
    a.lda(abs(sym("statement_high") + base).x());
    a.pha();
    a.lda(abs(sym("statement_low") + base).x());
    a.pha();
    a.jmp(abs(zp::CHRGET));
    a.bind(syntax);
    a.cmp(imm(token::GO));
    a.bne("syntax_jump");
    a.jsr(zp::CHRGET);
    a.lda(imm(token::TO));
    a.jsr("expect_char");
    a.jmp("goto_line");
    a.here("syntax_jump");
    a.jmp("syntax_error");

    // TXTPTR, or the pointer in zero page X, moved on A bytes.
    a.here("add_txtptr");
    a.ldx(imm(lo(zp::TXTPTR)));
    a.here("add_to_pointer");
    a.clc();
    a.adc(z(0).x());
    a.sta(z(0).x());
    a.bcc(RTS);
    a.inc(z(1).x());
    a.rts();

    // A comma at TXTPTR: the character after it, C clear; else C set.
    a.here("comma_next");
    a.jsr(zp::CHRGOT);
    a.cmp(imm(b','));
    a.sec();
    a.bne(RTS);
    a.jsr(zp::CHRGET);
    a.clc();
    a.rts();

    // At the keyboard, ILLEGAL DIRECT.
    a.here("check_program");
    a.ldx(z(zp::CURLIN + 1));
    a.inx();
    a.bne(RTS);
    raise(a, number::ILLEGAL_DIRECT);
}

/// END, STOP and BREAK keep their place for CONT — END quietly, STOP and
/// BREAK as BREAK and the line — and CONT resumes it; the program's end
/// leaves nothing to resume.
fn ends(a: &mut Asm) {
    let stop_or_end = a.local();
    let direct = a.local();
    a.here("st_stop");
    a.bne(RTS);
    a.here("break");
    a.sec();
    a.bcs(stop_or_end);
    a.here("st_end");
    a.bne(RTS);
    a.clc();
    a.bind(stop_or_end);
    let quietly = a.local();
    a.ldx(z(zp::CURLIN + 1));
    a.inx();
    a.beq(direct);
    a.ldx(imm(1));
    let keep = a.here_local();
    a.lda(z(zp::TXTPTR).x());
    a.sta(z(zp::OLDTXT).x());
    a.lda(z(zp::CURLIN).x());
    a.sta(z(zp::OLDLIN).x());
    a.dex();
    a.bpl(keep);
    a.bind(direct);
    a.bcc(quietly);
    a.ldx(imm(number::BREAK));
    a.jmp("error");
    a.bind(quietly);
    a.jmp("ready");

    a.here("st_cont");
    let possible = a.local();
    a.bne(RTS);
    a.ldy(z(zp::OLDTXT + 1));
    a.bne(possible);
    raise(a, number::CANT_CONTINUE);
    a.bind(possible);
    a.lda(z(zp::OLDTXT));
    a.sta(z(zp::TXTPTR));
    a.sty(z(zp::TXTPTR + 1));
    a.lda(z(zp::OLDLIN));
    a.sta(z(zp::CURLIN));
    a.lda(z(zp::OLDLIN + 1));
    a.sta(z(zp::CURLIN + 1));
    a.rts();
}

/// RUN: CLR, then the program from its start or from a line. CLR. NEW.
fn run(a: &mut Asm) {
    a.here("st_run");
    let from_start = a.local();
    a.lda(imm(0));
    a.jsr(kernal::SETMSG);
    // The stack goes, so the character is read again after.
    a.jsr("clear_all");
    a.jsr(zp::CHRGOT);
    a.beq(from_start);
    a.jmp("goto_next_statement");
    a.bind(from_start);
    a.jsr("text_start");
    a.jmp("next_statement");

    a.here("st_clr");
    a.bne(RTS);
    a.jsr("clear_all");
    a.jmp("next_statement");

    a.here("st_new");
    a.bne(RTS);
    a.jsr("new_program");
    a.jsr("cont_start");
    a.jmp("ready");
}

/// GOTO's `goto_line`: TXTPTR before the line numbered at TXTPTR,
/// searched from the next line when it lies ahead, else from the start;
/// none is UNDEF'D STATEMENT. GOSUB: a frame of CURLIN and TXTPTR, then
/// the line. RETURN: the latest GOSUB's frame, the FOR frames above it
/// dropped, its line and place restored and the rest of the GOSUB
/// statement passed over.
fn jumps(a: &mut Asm) {
    a.here("goto_line");
    let from_start = a.local();
    let search = a.local();
    let found = a.local();
    a.jsr("parse_line_number");
    a.lda(z(zp::CURLIN));
    a.cmp(z(zp::LINNUM));
    a.lda(z(zp::CURLIN + 1));
    a.sbc(z(zp::LINNUM + 1));
    a.bcs(from_start);
    a.ldx(imm(0));
    a.jsr("scan_to");
    a.tya();
    a.sec();
    a.adc(z(zp::TXTPTR));
    a.ldx(z(zp::TXTPTR + 1));
    a.bcc(search);
    a.inx();
    a.bcs(search);
    a.bind(from_start);
    a.lda(z(zp::TXTTAB));
    a.ldx(z(zp::TXTTAB + 1));
    a.bind(search);
    a.jsr("find_line_from");
    a.bcs(found);
    raise(a, number::UNDEFD_STATEMENT);
    a.bind(found);
    a.lda(z(LINE_FOUND));
    a.sbc(imm(1));
    a.sta(z(zp::TXTPTR));
    a.lda(z(LINE_FOUND + 1));
    a.sbc(imm(0));
    a.sta(z(zp::TXTPTR + 1));
    a.rts();

    a.here("st_gosub");
    a.pla();
    a.pla();
    a.lda(imm(GOSUB_FRAME));
    a.jsr("check_stack");
    for byte in [zp::TXTPTR + 1, zp::TXTPTR, zp::CURLIN + 1, zp::CURLIN] {
        a.lda(z(byte));
        a.pha();
    }
    a.lda(imm(token::GOSUB));
    a.pha();
    a.jsr(zp::CHRGOT);
    a.here("goto_next_statement");
    a.jsr("goto_line");
    a.jmp("next_statement");

    // A bytes of stack to be free past the reserve, else OUT OF MEMORY.
    a.here("check_stack");
    a.clc();
    a.adc(imm(STACK_RESERVE));
    a.sta(z(zp::INDEX));
    a.tsx();
    a.cpx(z(zp::INDEX));
    a.bcs(RTS);
    a.jmp("out_of_memory_error");

    a.here("st_return");
    let gosub = a.local();
    a.pla();
    a.pla();
    a.lda(imm(0xFF));
    a.sta(z(zp::FORPNT + 1));
    a.jsr("find_frame");
    a.txs();
    a.cmp(imm(token::GOSUB));
    a.beq(gosub);
    raise(a, number::RETURN_WITHOUT_GOSUB);
    a.bind(gosub);
    a.pla();
    for byte in [zp::CURLIN, zp::CURLIN + 1, zp::TXTPTR, zp::TXTPTR + 1] {
        a.pla();
        a.sta(z(byte));
    }
    a.jsr("st_data");
    a.jmp("next_statement");
}

// ON's two keywords differ in one bit, so one mask tells them from all others.
const _: () = assert!((token::GOTO ^ token::GOSUB).is_power_of_two());

/// IF: the condition — a number not zero, a string not empty — then the
/// statement or line after THEN, or GOTO's line; false, the rest of the
/// line is passed over. ON: the expression's line of GOTO's or GOSUB's
/// list, none past its end or for zero.
fn conditions(a: &mut Asm) {
    a.here("st_if");
    let test = a.local();
    let numeric = a.local();
    let truth = a.local();
    let statement = a.local();
    a.jsr("evaluate");
    a.jsr(zp::CHRGOT);
    a.cmp(imm(token::GOTO));
    a.beq(test);
    a.lda(imm(token::THEN));
    a.jsr("expect_char");
    a.bind(test);
    a.lda(z(zp::VALTYP));
    a.beq(numeric);
    a.jsr("free_fac_string");
    a.jmp(truth);
    a.bind(numeric);
    a.lda(z(zp::FACEXP));
    a.bind(truth);
    a.bne(statement);
    a.jmp("st_rem");
    a.bind(statement);
    let line = a.local();
    a.jsr(zp::CHRGOT);
    a.bcc(line);
    a.jmp("execute");
    a.bind(line);
    a.jmp("goto_line");

    a.here("st_on");
    let statement_ok = a.local();
    let target = a.local();
    let take = a.local();
    a.jsr("evaluate_byte");
    a.sta(z(ON_STATEMENT));
    a.eor(imm(token::GOTO));
    a.and(imm(!(token::GOTO ^ token::GOSUB)));
    a.beq(statement_ok);
    a.jmp("syntax_error");
    a.bind(statement_ok);
    a.stx(z(ON_COUNT));
    a.bind(target);
    a.dec(z(ON_COUNT));
    a.beq(take);
    a.jsr(zp::CHRGET);
    a.jsr("parse_line_number");
    a.cmp(imm(b','));
    a.beq(target);
    a.rts();
    // The token's statement, its target the next item.
    a.bind(take);
    a.lda(z(ON_STATEMENT));
    a.jmp("execute");
}

/// REM: TXTPTR at the line's end. DATA: TXTPTR at the statement's end, a
/// colon outside quotes or the line's end. `scan_statement`: Y the
/// statement's end from TXTPTR, which stays.
fn skips(a: &mut Asm) {
    a.here("st_rem");
    a.ldx(imm(0));
    a.skip_over(|a| {
        a.here("st_data");
        a.ldx(imm(b':'));
    });
    a.jsr("scan_to");
    a.tya();
    a.jmp("add_txtptr");

    a.movable(|a| {
        a.here("scan_statement");
        a.ldx(imm(b':'));
        a.here("scan_to");
        let step = a.local();
        let quoted = a.local();
        a.stx(z(zp::CHARAC));
        a.ldy(imm(0));
        a.bind(step);
        a.lda(z(zp::TXTPTR).ind_y());
        a.beq(RTS);
        a.cmp(z(zp::CHARAC));
        a.beq(RTS);
        a.iny();
        a.cmp(imm(b'"'));
        a.bne(step);
        // Within quotes only the line's end stops it.
        a.bind(quoted);
        a.lda(z(zp::TXTPTR).ind_y());
        a.beq(RTS);
        a.iny();
        a.cmp(imm(b'"'));
        a.bne(quoted);
        a.beq(step);
        a.rts();
    });
}

/// `find_frame`: X the frame above the caller's return of the FOR whose
/// variable is FORPNT, or of the latest FOR where FORPNT's high byte is
/// zero, with Z and C set; else Z clear, A the first frame's token past
/// the FORs, or `$FF` past them all.
fn frames(a: &mut Asm) {
    a.here("find_frame");
    let search = a.local();
    let next = a.local();
    let none = a.local();
    a.tsx();
    a.inx();
    a.inx();
    a.bind(search);
    a.cpx(imm(STACK_BASE));
    a.bcs(none);
    a.lda(abs(frame::TOKEN).x());
    a.cmp(imm(token::FOR));
    a.bne(RTS);
    a.lda(z(zp::FORPNT + 1));
    a.beq(RTS);
    a.lda(abs(frame::VARIABLE).x());
    a.cmp(z(zp::FORPNT));
    a.bne(next);
    a.lda(abs(frame::VARIABLE + 1).x());
    a.cmp(z(zp::FORPNT + 1));
    a.beq(RTS);
    a.bind(next);
    a.txa();
    a.clc();
    a.adc(imm(FOR_FRAME));
    a.tax();
    a.bne(search);
    a.bind(none);
    a.lda(imm(0xFF));
    a.rts();
}

/// FOR: the variable assigned its start, a number; a frame for the same
/// variable and those above it dropped; then a frame of where the loop
/// resumes, its limit, its step (1 unless given) and the step's sign.
fn for_loop(a: &mut Asm) {
    a.here("st_for");
    let fresh = a.local();
    let stepped = a.local();
    a.ldx(imm(SUBSCRIPTS_REFUSED));
    a.stx(z(zp::SUBFLG));
    a.jsr("st_let");
    // The return to the statement loop dropped: the frame takes its place.
    a.pla();
    a.pla();
    a.jsr("check_numeric");
    a.jsr("find_frame");
    a.bne(fresh);
    a.txa();
    a.adc(imm(FOR_FRAME - 1));
    a.tax();
    a.txs();
    a.bind(fresh);
    a.lda(imm(FOR_FRAME));
    a.jsr("check_stack");
    // Where NEXT resumes: past this statement.
    a.jsr("scan_statement");
    a.tya();
    a.clc();
    a.adc(z(zp::TXTPTR));
    a.tax();
    a.lda(z(zp::TXTPTR + 1));
    a.adc(imm(0));
    a.pha();
    a.txa();
    a.pha();
    a.lda(z(zp::CURLIN + 1));
    a.pha();
    a.lda(z(zp::CURLIN));
    a.pha();
    a.lda(imm(token::TO));
    a.jsr("expect_char");
    a.jsr("evaluate_number");
    // Room for the step, its sign and the limit, the limit stored at its
    // top while the step is evaluated beneath; STEP 1 unless given.
    a.tsx();
    a.txa();
    a.sec();
    a.sbc(imm(STEP_TO_LIMIT));
    a.tax();
    a.txs();
    a.adc(imm(frame::LIMIT - frame::STEP));
    a.tax();
    a.ldy(imm(hi(STACK_PAGE)));
    a.jsr("fac_store");
    a.ldy(imm(1));
    a.jsr("byte_to_fac");
    a.jsr(zp::CHRGOT);
    a.cmp(imm(token::STEP));
    a.bne(stepped);
    a.jsr(zp::CHRGET);
    a.jsr("evaluate_number");
    a.bind(stepped);
    a.jsr("sign");
    a.tsx();
    a.sta(abs(frame::STEP_SIGN + 1 - u16::from(frame::STEP)).x());
    a.inx();
    a.ldy(imm(hi(STACK_PAGE)));
    a.jsr("fac_store");
    a.lda(z(zp::FORPNT + 1));
    a.pha();
    a.lda(z(zp::FORPNT));
    a.pha();
    a.lda(imm(token::FOR));
    a.pha();
    a.jmp("next_statement");
}

/// A packed real's bytes, and a FOR frame's bytes from its step to its
/// limit's end.
const REAL: u8 = 5;
const STEP_TO_LIMIT: u8 = frame::LIMIT + REAL - frame::STEP;

/// `find_variable` refuses a subscript or an integer while SUBFLG has this.
pub const SUBSCRIPTS_REFUSED: u8 = 0x80;

/// NEXT: the frame of the variable named, or the latest; the variable
/// plus the step; while not past the limit — the comparison not the
/// step's sign — the loop resumes; otherwise the frame goes and a comma
/// names the next variable.
fn next_loop(a: &mut Asm) {
    a.here("st_next");
    let variable = a.local();
    let named = a.local();
    let search = a.local();
    let found = a.local();
    let done = a.local();
    a.pla();
    a.pla();
    a.jsr(zp::CHRGOT);
    a.bind(variable);
    a.bne(named);
    a.lda(imm(0));
    a.sta(z(zp::FORPNT + 1));
    a.beq(search);
    a.bind(named);
    a.jsr("find_variable");
    a.sta(z(zp::FORPNT));
    a.sty(z(zp::FORPNT + 1));
    a.bind(search);
    a.jsr("find_frame");
    a.beq(found);
    raise(a, number::NEXT_WITHOUT_FOR);
    a.bind(found);
    a.txs();
    a.lda(abs(frame::VARIABLE).x());
    a.ldy(abs(frame::VARIABLE + 1).x());
    a.jsr("fac_load");
    a.tsx();
    a.txa();
    a.clc();
    a.adc(imm(frame::STEP));
    a.ldy(imm(1));
    a.jsr("fadd_mem");
    a.tsx();
    a.lda(abs(frame::VARIABLE).x());
    a.ldy(abs(frame::VARIABLE + 1).x());
    a.tax();
    a.jsr("fac_store");
    a.tsx();
    a.txa();
    a.clc();
    a.adc(imm(frame::LIMIT));
    a.ldy(imm(1));
    a.jsr("compare_fac");
    a.tsx();
    a.cmp(abs(frame::STEP_SIGN).x());
    a.beq(done);
    for (from, to) in [(frame::LINE, zp::CURLIN), (frame::TEXT, zp::TXTPTR)] {
        a.lda(abs(from).x());
        a.sta(z(to));
        a.lda(abs(from + 1).x());
        a.sta(z(to + 1));
    }
    a.jmp("next_statement");
    a.bind(done);
    a.txa();
    a.adc(imm(FOR_FRAME - 1));
    a.tax();
    a.txs();
    a.jsr("comma_next");
    a.bcc(variable);
    a.jmp("next_statement");
}

/// POKE address, value. WAIT address, mask, and the bits inverted first:
/// until a masked bit is set. SYS address: the registers from SAREG to
/// SPREG, the routine called, the registers kept there again.
/// `address_and_byte`: LINNUM the address, X the byte after a comma.
fn memory(a: &mut Asm) {
    a.here("address_and_byte");
    a.jsr("getadr_expression");
    a.jsr("expect_comma");
    a.jmp("evaluate_byte");

    a.here("st_poke");
    a.jsr("address_and_byte");
    a.txa();
    a.ldy(imm(0));
    a.sta(z(zp::LINNUM).ind_y());
    a.rts();

    a.here("st_wait");
    let inverted = a.local();
    let poll = a.local();
    a.jsr("address_and_byte");
    a.stx(z(WAIT_MASK));
    a.jsr("optional_byte");
    a.bcc(inverted);
    a.lda(imm(0));
    a.bind(inverted);
    a.sta(z(WAIT_INVERT));
    a.ldy(imm(0));
    a.bind(poll);
    a.lda(z(zp::LINNUM).ind_y());
    a.eor(z(WAIT_INVERT));
    a.and(z(WAIT_MASK));
    a.beq(poll);
    a.rts();

    a.here("st_sys");
    a.jsr("getadr_expression");
    a.lda(abs(page::SPREG));
    a.pha();
    a.lda(abs(page::SAREG));
    a.ldx(abs(page::SXREG));
    a.ldy(abs(page::SYREG));
    a.plp();
    a.jsr("sys_call");
    a.php();
    a.sta(abs(page::SAREG));
    a.stx(abs(page::SXREG));
    a.sty(abs(page::SYREG));
    a.pla();
    a.sta(abs(page::SPREG));
    a.rts();
    a.here("sys_call");
    a.jmp(abs(zp::LINNUM).ind());
}
