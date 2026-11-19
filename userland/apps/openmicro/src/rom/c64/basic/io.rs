//! Input and output: printing through CHROUT, PRINT, PRINT# and CMD, GET,
//! INPUT and READ with the items they share, OPEN and CLOSE, LOAD, SAVE
//! and VERIFY, and PEEK, FRE, POS and USR.

use super::errors::{number, raise, DIRECT_MODE};
use super::strings::NEW_LENGTH;
use super::tokens::token;
use super::ws::{page, zp};
use crate::cpu::STACK_PAGE;
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Asm, Label, RTS};
use crate::rom::c64::kernal::entry as kernal;
use crate::rom::c64::kernal::files::{device, message, status, NO_SECONDARY};
use crate::rom::c64::kernal::ws::zp as kzp;
use crate::rom::c64::petscii::CURSOR_RIGHT;

/// PRINT's comma zones.
const ZONE: u8 = 10;
/// INPFLG: what is reading items.
mod reader {
    pub const INPUT: u8 = 0x00;
    pub const READ: u8 = 0x98;
}
/// INPFLG once INPUT has given a variable an item.
const ITEM_TAKEN: u8 = 0x01;
/// INPUT's stack at the statement's start, for ?REDO FROM START.
const INPUT_STACK: u16 = zp::TRMPOS;
/// The messages INPUT shows.
const REDO: &str = "?REDO FROM START\r";
const EXTRA: &str = "?EXTRA IGNORED\r";
const PROMPT: &str = "? ";
/// The device and secondary address OPEN, LOAD and SAVE assume.
const DEFAULT_DEVICE: u8 = device::TAPE;
const DEFAULT_SECONDARY: u8 = 0;

pub fn emit(a: &mut Asm) {
    a.movable(|a| {
        output(a);
        channels(a);
        print(a);
    });
    a.movable(|a| {
        reading(a);
        items(a);
    });
    a.movable(|a| {
        let syntax = a.local();
        fetching(a);
        parsing(a, syntax);
        getting(a, syntax);
    });
    a.movable(|a| {
        files(a);
        file_arguments(a);
        loads(a);
    });
    a.movable(memory_functions);
    a.later(|a| {
        for (name, text) in [
            ("redo_text", REDO),
            ("extra_text", EXTRA),
            ("prompt_text", PROMPT),
        ] {
            a.here(name);
            a.bytes(text.as_bytes());
            a.byte(0);
        }
    });
}

/// `print_char`: A through CHROUT. `print_string`: the text at A (low) and
/// Y to its zero. `print_gap`: a cursor right on the screen, a space
/// elsewhere; `print_gaps`: A of them, CHROUT keeping Y. `print_prompt`:
/// `? `, unless a file has the output. `restore_output`: no CMD, output to
/// the screen.
fn output(a: &mut Asm) {
    a.here("print_gaps");
    let gap = a.local();
    a.tay();
    a.beq(RTS);
    a.bind(gap);
    a.jsr("print_gap");
    a.dey();
    a.bne(gap);
    a.rts();

    a.here("print_gap");
    let space = a.local();
    a.lda(imm(b' '));
    a.ldx(z(kzp::DFLTO));
    a.cpx(imm(device::SCREEN));
    a.bne(space);
    a.lda(imm(CURSOR_RIGHT));
    a.bind(space);
    a.fall_into("print_char");
    a.here("print_char");
    a.jmp(abs(kernal::CHROUT));

    a.here("print_prompt");
    a.lda(z(zp::CHANNL));
    a.bne(RTS);
    a.lda(imm(lo("prompt_text")));
    a.ldy(imm(hi("prompt_text")));
    a.here("print_string");
    let next = a.local();
    a.sta(z(zp::INDEX));
    a.sty(z(zp::INDEX + 1));
    a.ldy(imm(0));
    a.bind(next);
    a.lda(z(zp::INDEX).ind_y());
    a.beq(RTS);
    a.jsr("print_char");
    a.iny();
    a.bne(next);
    a.rts();

    a.here("restore_output");
    a.lda(imm(0));
    a.sta(z(zp::CHANNL));
    a.jmp(abs(kernal::CLRCHN));
}

/// PRINT: its items — an expression, a number followed by a gap; TAB to a
/// column, SPC gaps, a comma to the next zone of ten, each as a semicolon
/// after it — then a new line unless a semicolon or comma ends it.
/// `print_value`: FAC printed, a number as `fac_to_text` writes it and a gap;
/// `print_fac_string`: FAC's string freed and printed; `print_text`: A
/// characters of the text at INDEX.
fn print(a: &mut Asm) {
    a.here("st_print");
    let item = a.local();
    let tab = a.local();
    let comma = a.local();
    let zone = a.local();
    let spc = a.local();
    let gaps = a.local();
    let separator = a.local();
    a.bind(item);
    a.beq("print_newline");
    a.cmp(imm(token::SPC));
    a.beq(spc);
    a.cmp(imm(token::TAB));
    a.beq(tab);
    a.cmp(imm(b','));
    a.beq(comma);
    a.cmp(imm(b';'));
    a.beq(separator);
    a.jsr("evaluate");
    a.jsr("print_value");
    a.jsr(zp::CHRGOT);
    a.jmp(item);
    // C set by the argument's bracket, and by the comma.
    a.bind(tab);
    a.jsr("print_tab_argument");
    a.sbc(z(kzp::PNTR));
    a.bcs(gaps);
    a.bcc(separator);
    a.bind(comma);
    a.lda(z(kzp::PNTR));
    a.bind(zone);
    a.sbc(imm(ZONE));
    a.bcs(zone);
    a.eor(imm(0xFF));
    a.adc(imm(1));
    a.bne(gaps);
    a.bind(spc);
    a.jsr("print_tab_argument");
    a.bind(gaps);
    a.jsr("print_gaps");
    a.bind(separator);
    a.jsr(zp::CHRGET);
    a.bne(item);
    a.rts();

    // TAB('s and SPC('s argument and its bracket: A the byte, C set.
    a.here("print_tab_argument");
    let closed = a.local();
    a.jsr(zp::CHRGET);
    a.jsr("evaluate_byte");
    a.cmp(imm(b')'));
    a.beq(closed);
    a.jmp("syntax_error");
    a.bind(closed);
    a.txa();
    a.rts();

    a.here("print_value");
    a.lda(z(zp::VALTYP));
    a.bne("print_fac_string");
    a.jsr("fac_to_text");
    a.jsr("print_string");
    a.jmp("print_gap");
    a.here("print_fac_string");
    a.jsr("free_fac_string");
    a.here("print_text");
    let next = a.local();
    a.sta(z(zp::CHARAC));
    a.ldy(imm(0));
    a.bind(next);
    a.cpy(z(zp::CHARAC));
    a.beq(RTS);
    a.lda(z(zp::INDEX).ind_y());
    a.jsr("print_char");
    a.iny();
    a.bne(next);
    a.rts();
}

/// PRINT# and CMD: output to a file, then PRINT; PRINT# ends with output
/// back on the screen, CMD leaves it there. `output_file` and
/// `input_file`: the file numbered at TXTPTR made the output or the input,
/// CHANNL its number; `input_file` then its comma. `print_newline`: a
/// return, and a line feed for a file numbered 128 or more.
fn channels(a: &mut Asm) {
    a.here("st_print_file");
    a.jsr("output_file");
    a.jsr("print_after_file");
    a.jmp("restore_output");

    a.here("output_file");
    a.jsr("evaluate_byte");
    a.stx(z(zp::CHANNL));
    a.jsr(kernal::CHKOUT);
    a.jmp("kernal_check");

    a.here("input_file");
    a.jsr("evaluate_byte");
    a.stx(z(zp::CHANNL));
    a.jsr(kernal::CHKIN);
    a.jsr("kernal_check");
    a.jmp("expect_comma");

    a.here("print_newline");
    a.lda(imm(b'\r'));
    a.jsr("print_char");
    a.bit(z(zp::CHANNL));
    a.bpl(RTS);
    a.lda(imm(b'\n'));
    a.jmp("print_char");

    a.here("st_cmd");
    a.jsr("output_file");
    a.fall_into("print_after_file");
    a.here("print_after_file");
    a.jsr(zp::CHRGOT);
    a.beq("print_newline");
    a.jsr("expect_comma");
    a.fall_into("st_print");
}

/// INPUT: in a program only, a prompt and `? `, a line read, its items
/// given to the variables — `?? ` for more, an empty reply leaving them,
/// ?REDO FROM START for a number that is not, ?EXTRA IGNORED for more
/// than they take. INPUT#: the same from a file, quietly, and in a program
/// only once the file and the comma after it are found; a first line that
/// a RETURN opens is passed over as the original's is, so a file at its
/// end holds it there; one the device timed out on is taken. READ: from
/// the DATA statements in turn.
fn reading(a: &mut Asm) {
    a.here("st_input_file");
    let (line, taken) = (a.local(), a.local());
    a.jsr("input_file");
    a.jsr("check_program");
    a.bind(line);
    a.jsr("read_line_quietly");
    a.lda(abs(page::BUF));
    a.bne(taken);
    a.lda(z(kzp::STATUS));
    a.and(imm(status::READ_TIMEOUT));
    a.beq(line);
    a.bind(taken);
    a.jsr("input_items");
    a.jmp("restore_output");

    a.here("st_read");
    a.ldx(z(zp::DATPTR));
    a.ldy(z(zp::DATPTR + 1));
    a.lda(imm(reader::READ));
    a.jsr("read_items");
    a.lda(z(zp::INPPTR));
    a.sta(z(zp::DATPTR));
    a.lda(z(zp::INPPTR + 1));
    a.sta(z(zp::DATPTR + 1));
    a.rts();

    // GET, or GET# with a file's number.

    a.here("st_input");
    let no_prompt = a.local();
    a.tsx();
    a.stx(z(INPUT_STACK));
    a.jsr("check_program");
    a.cmp(imm(b'"'));
    a.bne(no_prompt);
    a.jsr("str_literal");
    a.lda(imm(b';'));
    a.jsr("expect_char");
    a.jsr("print_fac_string");
    a.bind(no_prompt);
    a.jsr("print_prompt");
    a.jsr("read_line");
    a.here("input_items");
    a.lda(imm(reader::INPUT));
    a.fall_into("read_items");
}

/// `read_items`: from the data before X (low) and Y, for INPFLG A, each
/// variable at TXTPTR given the next item, INPPTR left after the last;
/// where INPUT finds no more, the rest keep their values. `read_variable`:
/// one, C set where INPUT found no more. `swap_pointers`: TXTPTR and
/// INPPTR exchanged, C kept.
fn items(a: &mut Asm) {
    a.here("read_items");
    let variable = a.local();
    let rest_kept = a.local();
    a.sta(z(zp::INPFLG));
    a.stx(z(zp::INPPTR));
    a.sty(z(zp::INPPTR + 1));
    a.jsr(zp::CHRGOT);
    a.bind(variable);
    a.jsr("read_variable");
    a.bcs(rest_kept);
    a.jsr("comma_next");
    a.bcc(variable);
    a.lda(z(zp::INPFLG));
    a.cmp(imm(reader::INPUT | ITEM_TAKEN));
    a.bne(RTS);
    a.ldy(imm(0));
    a.lda(z(zp::INPPTR).ind_y());
    a.beq(RTS);
    a.lda(z(zp::CHANNL));
    a.bne(RTS);
    a.lda(imm(lo("extra_text")));
    a.ldy(imm(hi("extra_text")));
    a.jmp("print_string");
    a.bind(rest_kept);
    a.jmp("st_data");

    a.here("read_variable");
    let none = a.local();
    a.jsr("find_variable");
    a.sta(z(zp::FORPNT));
    a.sty(z(zp::FORPNT + 1));
    a.jsr("swap_pointers");
    a.jsr("fetch_item");
    a.bcs(none);
    a.jsr("parse_item");
    a.jsr("assign");
    a.lda(z(zp::INPFLG));
    a.ora(imm(ITEM_TAKEN));
    a.sta(z(zp::INPFLG));
    a.clc();
    a.bind(none);
    a.fall_into("swap_pointers");

    a.here("swap_pointers");
    let byte = a.local();
    a.ldx(imm(1));
    a.bind(byte);
    a.lda(z(zp::TXTPTR).x());
    a.ldy(z(zp::INPPTR).x());
    a.sta(z(zp::INPPTR).x());
    a.sty(z(zp::TXTPTR).x());
    a.dex();
    a.bpl(byte);
    a.rts();
}

/// `fetch_item`: the next item's first character at TXTPTR. READ looks at
/// the separator it stands on — a comma, or anything but a statement's
/// end, steps on; at the end it goes to the next DATA statement, OUT OF
/// DATA past the last. INPUT steps from the start of its reply; at the
/// end, with an item taken, it asks `?? ` for more, quietly from a file,
/// and an empty reply leaves the rest. C set where there is none.
/// `find_data`: TXTPTR at the next DATA statement's token, DATLIN its
/// line.
fn fetching(a: &mut Asm) {
    // READ past its DATA statement: on to the next, then again.
    let read_next = a.here_local();
    a.jsr("find_data");
    a.here("fetch_item");
    let again = a.local();
    let check = a.local();
    let advance = a.local();
    let no_data = a.local();
    let quietly = a.local();
    let fresh = a.local();
    let none = a.local();
    a.bind(again);
    a.lda(z(zp::INPFLG));
    a.bmi(check);
    a.and(imm(ITEM_TAKEN));
    a.beq(advance);
    a.bind(check);
    a.ldy(imm(0));
    a.lda(z(zp::TXTPTR).ind_y());
    a.beq(no_data);
    a.cmp(imm(b':'));
    a.beq(no_data);
    a.bind(advance);
    a.jsr(zp::CHRGET);
    a.beq(no_data);
    a.clc();
    a.rts();
    a.bind(no_data);
    a.lda(z(zp::INPFLG));
    a.bmi(read_next);
    a.and(imm(ITEM_TAKEN));
    a.beq(none);
    a.lda(z(zp::CHANNL));
    a.bne(quietly);
    a.lda(imm(b'?'));
    a.jsr("print_char");
    a.jsr("print_prompt");
    a.jsr("read_line");
    a.jmp(fresh);
    a.bind(quietly);
    a.jsr("read_line_quietly");
    a.bind(fresh);
    a.stx(z(zp::TXTPTR));
    a.sty(z(zp::TXTPTR + 1));
    a.lda(abs(page::BUF));
    a.bne(advance);
    a.bind(none);
    a.sec();
    a.rts();

    a.here("find_data");
    let statement = a.local();
    let colon = a.local();
    let line = a.local();
    a.bind(statement);
    a.jsr("st_data");
    a.ldy(imm(0));
    a.lda(z(zp::TXTPTR).ind_y());
    a.bne(colon);
    a.ldy(imm(2));
    a.lda(z(zp::TXTPTR).ind_y());
    a.bne(line);
    raise(a, number::OUT_OF_DATA);
    a.bind(line);
    a.iny();
    a.lda(z(zp::TXTPTR).ind_y());
    a.sta(z(zp::DATLIN));
    a.iny();
    a.lda(z(zp::TXTPTR).ind_y());
    a.sta(z(zp::DATLIN + 1));
    a.tya();
    a.jsr("add_txtptr");
    a.bind(colon);
    a.jsr(zp::CHRGET);
    a.cmp(imm(token::DATA));
    a.bne(statement);
    a.rts();
}

/// `parse_item`: the item at TXTPTR as the variable's type takes it — a number
/// by `text_to_fac`; a string quoted, or to the next comma or colon, left where
/// it is — which a comma or a statement's end must follow: else SYNTAX in
/// READ's DATA line, FILE DATA from a file, and at the keyboard ?REDO FROM
/// START, which runs the INPUT again.
fn parsing(a: &mut Asm, syntax_jump: Label) {
    a.here("parse_item");
    let syntax = a.local();
    let file_data = a.local();
    a.jsr("parse_value");
    a.jsr(zp::CHRGOT);
    a.beq(RTS);
    a.cmp(imm(b','));
    a.beq(RTS);
    a.bit(z(zp::INPFLG));
    a.bmi(syntax);
    a.lda(z(zp::CHANNL));
    a.bne(file_data);
    a.lda(imm(lo("redo_text")));
    a.ldy(imm(hi("redo_text")));
    a.jsr("print_string");
    a.ldx(z(INPUT_STACK));
    a.inx();
    a.inx();
    a.txs();
    a.lda(z(zp::OLDTXT));
    a.sta(z(zp::TXTPTR));
    a.lda(z(zp::OLDTXT + 1));
    a.sta(z(zp::TXTPTR + 1));
    a.jmp("next_statement");
    a.bind(syntax);
    a.lda(z(zp::DATLIN));
    a.sta(z(zp::CURLIN));
    a.lda(z(zp::DATLIN + 1));
    a.sta(z(zp::CURLIN + 1));
    a.bind(syntax_jump);
    a.jmp("syntax_error");
    a.bind(file_data);
    raise(a, number::FILE_DATA);

    a.here("parse_value");
    let string = a.local();
    let unquoted = a.local();
    let scan = a.local();
    let end = a.local();
    a.ldx(z(zp::VALTYP));
    a.bne(string);
    a.jmp("text_to_fac");
    a.bind(string);
    a.cmp(imm(b'"'));
    a.bne(unquoted);
    a.jmp("str_literal");
    a.bind(unquoted);
    a.ldy(imm(0));
    a.bind(scan);
    a.lda(z(zp::TXTPTR).ind_y());
    a.beq(end);
    a.cmp(imm(b','));
    a.beq(end);
    a.cmp(imm(b':'));
    a.beq(end);
    a.iny();
    a.bne(scan);
    a.bind(end);
    a.clc();
    a.jmp("text_temp");
}

/// GET and GET#, in a program only, its file checked after that: each
/// variable from GETIN a string the character, or empty for none; a number
/// its digit, 0 for none, SYNTAX for another character.
fn getting(a: &mut Asm, syntax: Label) {
    a.here("st_get");
    a.jsr("check_program");
    a.cmp(imm(b'#'));
    a.bne("get_items");
    a.jsr(zp::CHRGET);
    a.jsr("input_file");
    a.jsr("get_items");
    a.jmp("restore_output");

    a.here("get_items");
    let variable = a.local();
    let string = a.local();
    let digit = a.local();
    let store = a.local();
    let empty = a.local();
    a.bind(variable);
    a.jsr("find_variable");
    a.sta(z(zp::FORPNT));
    a.sty(z(zp::FORPNT + 1));
    a.jsr(kernal::GETIN);
    a.sta(abs(page::BUF));
    a.ldx(z(zp::VALTYP));
    a.bne(string);
    a.tay();
    a.beq(digit);
    a.sec();
    a.sbc(imm(b'0'));
    a.cmp(imm(10));
    a.tay();
    a.bcs(syntax);
    a.bind(digit);
    a.jsr("byte_to_fac");
    a.jmp(store);
    a.bind(string);
    a.tay();
    a.beq(empty);
    a.lda(imm(1));
    a.bind(empty);
    a.sta(z(NEW_LENGTH));
    a.lda(imm(lo(page::BUF)));
    a.sta(z(zp::FRESPC));
    a.lda(imm(hi(page::BUF)));
    a.sta(z(zp::FRESPC + 1));
    a.jsr("push_temp");
    a.bind(store);
    a.jsr("assign");
    a.jsr("comma_next");
    a.bcc(variable);
    a.rts();
}

/// OPEN and CLOSE. OPEN takes a file's number, then a device, secondary
/// address and name, each optional in turn.
fn files(a: &mut Asm) {
    a.here("st_open");
    let open_now = a.local();
    a.jsr("no_name");
    a.jsr("evaluate_byte");
    a.txa();
    a.pha();
    a.lda(imm(DEFAULT_DEVICE));
    a.pha();
    a.lda(imm(NO_SECONDARY));
    a.pha();
    a.jsr("device_and_secondary");
    a.jsr(zp::CHRGOT);
    a.beq(open_now);
    a.jsr("expect_comma");
    a.jsr("set_name");
    a.bind(open_now);
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.pla();
    a.jsr(kernal::SETLFS);
    a.jsr(kernal::OPEN);
    a.jmp("kernal_check");

    a.here("st_close");
    a.jsr("evaluate_byte");
    a.txa();
    a.jsr(kernal::CLOSE);
    a.jmp("kernal_check");
}

/// `device_and_secondary`: the device and secondary address the caller
/// pushed, each replaced by a byte after a comma where one is given.
/// `optional_byte`: a byte after a comma in A, C clear; at the statement's
/// end, C set. `set_name`: the name the expression at TXTPTR makes, or, as
/// `no_name`, no name. `file_arguments`: LOAD's, SAVE's and VERIFY's name,
/// device and secondary address.
fn file_arguments(a: &mut Asm) {
    a.here("device_and_secondary");
    let done = a.local();
    a.jsr("optional_byte");
    a.bcs(done);
    a.tsx();
    a.sta(abs(STACK_PAGE + 4).x());
    a.jsr("optional_byte");
    a.bcs(done);
    a.tsx();
    a.sta(abs(STACK_PAGE + 3).x());
    a.bind(done);
    a.rts();

    a.here("optional_byte");
    a.jsr(zp::CHRGOT);
    a.beq(RTS);
    a.jsr("expect_comma");
    a.jsr("evaluate_byte");
    a.txa();
    a.clc();
    a.rts();

    a.here("set_name");
    a.jsr("evaluate");
    a.jsr("check_string");
    a.jsr("free_fac_string");
    a.ldx(z(zp::INDEX));
    a.ldy(z(zp::INDEX + 1));
    a.skip_over(|a| {
        a.here("no_name");
        a.lda(imm(0));
    });
    a.jmp(abs(kernal::SETNAM));

    a.here("file_arguments");
    let set = a.local();
    a.jsr("no_name");
    a.lda(imm(DEFAULT_DEVICE));
    a.pha();
    a.lda(imm(DEFAULT_SECONDARY));
    a.pha();
    a.jsr(zp::CHRGOT);
    a.beq(set);
    a.jsr("set_name");
    a.jsr("device_and_secondary");
    a.bind(set);
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.lda(imm(0));
    a.jmp(abs(kernal::SETLFS));
}

/// LOAD, SAVE and VERIFY, each with a name, device and secondary address.
/// LOAD in a program runs the new one from its start with the variables
/// kept; at the keyboard it sets the variables' start past it and READY
/// follows.
fn loads(a: &mut Asm) {
    a.here("st_save");
    a.jsr("file_arguments");
    a.ldx(z(zp::VARTAB));
    a.ldy(z(zp::VARTAB + 1));
    a.lda(imm(lo(zp::TXTTAB)));
    a.jsr(kernal::SAVE);
    a.jmp("kernal_check");

    a.here("st_verify");
    a.lda(imm(1));
    a.skip_over(|a| {
        a.here("st_load");
        a.lda(imm(0));
    });
    let loaded = a.local();
    let verified = a.local();
    let complete = a.local();
    let in_program = a.local();
    a.sta(z(zp::VERCK));
    a.jsr("file_arguments");
    a.lda(z(zp::VERCK));
    a.ldx(z(zp::TXTTAB));
    a.ldy(z(zp::TXTTAB + 1));
    a.jsr(kernal::LOAD);
    a.jsr("kernal_check");
    a.lda(z(zp::VERCK));
    a.beq(loaded);
    a.jsr(kernal::READST);
    a.and(imm(status::MISMATCH));
    a.beq(verified);
    raise(a, number::VERIFY);
    a.bind(verified);
    a.ldy(imm(message::OK));
    a.jmp("control_message");
    // X and Y the program's end; READST keeps them.
    a.bind(loaded);
    a.jsr(kernal::READST);
    a.and(imm(!status::END));
    a.beq(complete);
    a.jmp("load_error");
    a.bind(complete);
    a.lda(z(zp::CURLIN + 1));
    a.cmp(imm(DIRECT_MODE));
    a.bne(in_program);
    a.stx(z(zp::VARTAB));
    a.sty(z(zp::VARTAB + 1));
    a.jsr("link_program");
    a.jsr("clear_all");
    a.jmp("ready");
    a.bind(in_program);
    a.jsr("link_program");
    a.jsr("text_start");
    a.jsr("reset_stacks");
    a.jmp("next_statement");
}

/// PEEK, LINNUM kept as POKE and WAIT hold their address there; FRE —
/// the strings compacted, then the room between the arrays and the
/// strings as a signed 16-bit number — POS, the cursor's column, and USR
/// through its vector.
fn memory_functions(a: &mut Asm) {
    a.here("fn_peek");
    a.lda(z(zp::LINNUM + 1));
    a.pha();
    a.lda(z(zp::LINNUM));
    a.pha();
    a.jsr("fac_to_word");
    a.ldy(imm(0));
    a.lda(z(zp::LINNUM).ind_y());
    a.tay();
    a.pla();
    a.sta(z(zp::LINNUM));
    a.pla();
    a.sta(z(zp::LINNUM + 1));
    a.tya();
    a.jmp("byte_result");

    a.here("fn_fre");
    let numeric = a.local();
    a.lda(z(zp::VALTYP));
    a.beq(numeric);
    a.jsr("free_fac_string");
    a.bind(numeric);
    a.jsr("collect");
    a.lda(imm(0));
    a.sta(z(zp::VALTYP));
    a.lda(z(zp::FRETOP));
    a.sec();
    a.sbc(z(zp::STREND));
    a.tay();
    a.lda(z(zp::FRETOP + 1));
    a.sbc(z(zp::STREND + 1));
    a.jmp("word_to_fac");

    a.here("fn_pos");
    a.sec();
    a.jsr(kernal::PLOT);
    a.tya();
    a.jmp("byte_result");

    a.here("fn_usr");
    a.jmp(abs(page::USRPOK));
}
