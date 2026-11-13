//! PRINT and BASIC's character output: COUNT, WIDTH, the `@%` fields, TAB
//! and SPC, and numbers in decimal or hex.

use super::decimal::{FORMAT, PLACES};
use super::ws::{kind, page, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, zp, Acc, Asm, Label, RTS};
use crate::rom::os::entry;

const TAB: u8 = 0x8A;
const SPC: u8 = 0x89;
const ELSE: u8 = 0x8B;
const CR: u8 = 0x0D;
/// `@%`'s bytes: the field width, digits, format and STR$'s flag.
const AT_WIDTH: u16 = page::RESIDENT;
const AT_DIGITS: u16 = page::RESIDENT + 1;
const AT_FORMAT: u16 = page::RESIDENT + 2;
const AT_STR: u16 = page::RESIDENT + 3;
/// Whether the last item was a `;`, so the line goes on.
const GOES_ON: u16 = z::STATEMENT;

pub fn emit(a: &mut Asm) {
    let tab = printing(a);
    print_tab(a, tab);
    output(a);
    numbers(a);
    items(a);
    reading(a);
    data(a);
    restoring(a);
    let next = input(a);
    input_items(a, next);
}

/// INPUT's state: whether to ask with `?`, whether LINE was given, and
/// where the next item starts in its line; and the line's OSWORD 0 block.
const ASK: u16 = z::STATEMENT;
const WHOLE: u16 = ASK + 1;
const NEXT_ITEM: u16 = WHOLE + 1;
const LINE_BLOCK: u16 = NEXT_ITEM + 1;
/// The line INPUT reads, on the BASIC stack.
const LINE_ROOM: u8 = 240;
/// The DATA pointer, and the text an item is read from.
const ITEM: u16 = z::TEXT_B;
const DATA_TOKEN: u8 = 0xDC;
const LINE_TOKEN: u8 = 0x86;

fn items(a: &mut Asm) {
    // Z set if A ends a statement: a colon, a CR or ELSE.
    a.here("ends_statement");
    a.cmp(imm(b':'));
    a.beq(RTS);
    a.cmp(imm(CR));
    a.beq(RTS);
    a.cmp(imm(ELSE));
    a.rts();

    // A raw item at (ITEM),Y into the string buffer: spaces before it
    // dropped; quoted, to its closing quote with "" a quote; otherwise to a
    // comma or the line's end. Y left at the comma or CR after it.
    a.here("raw_item");
    let spaces = a.here_local();
    a.lda(zp(ITEM).ind_y());
    a.cmp(imm(b' '));
    let started = a.local();
    a.bne(started);
    a.iny();
    a.bne(spaces);
    a.bind(started);
    a.ldx(imm(0));
    a.cmp(imm(b'"'));
    let (plain, quoted) = (a.local(), a.local());
    a.bne(plain);
    a.iny();
    a.bind(quoted);
    let (unclosed, closed, put) = (a.local(), a.local(), a.local());
    a.lda(zp(ITEM).ind_y());
    a.cmp(imm(CR));
    a.beq(unclosed);
    a.iny();
    a.cmp(imm(b'"'));
    a.bne(put);
    a.lda(zp(ITEM).ind_y());
    a.cmp(imm(b'"'));
    a.bne(closed);
    a.iny();
    a.bind(put);
    a.sta(abs(page::STRING).x());
    a.inx();
    a.bne(quoted);
    a.bind(unclosed);
    a.jmp("err_missing_quote");
    // After the closing quote, to the comma or the end.
    a.bind(closed);
    let skip = a.here_local();
    a.lda(zp(ITEM).ind_y());
    let ended = a.local();
    a.cmp(imm(b','));
    a.beq(ended);
    a.cmp(imm(CR));
    a.beq(ended);
    a.iny();
    a.bne(skip);
    a.bind(plain);
    let character = a.here_local();
    a.lda(zp(ITEM).ind_y());
    a.cmp(imm(b','));
    a.beq(ended);
    a.cmp(imm(CR));
    a.beq(ended);
    a.sta(abs(page::STRING).x());
    a.iny();
    a.inx();
    a.bne(character);
    a.bind(ended);
    a.stx(zp(z::STRING_LENGTH));
    a.rts();
}

fn reading(a: &mut Asm) {
    // READ v[, v...]: each variable from the next DATA item, an expression
    // for a number and raw text for a string.
    a.here("statement_read");
    a.jsr("lvalue");
    let mistake = a.local();
    a.bcs(mistake);
    a.lda(zp(z::VAR));
    a.pha();
    a.lda(zp(z::VAR + 1));
    a.pha();
    a.lda(zp(z::VTYPE));
    a.pha();
    a.jsr("data_item");
    a.pla();
    a.sta(zp(z::VTYPE));
    a.pha();
    let (string, stored) = (a.local(), a.local());
    a.bmi(string);
    // A number: the item evaluated where it lies.
    a.lda(zp(z::OFFSET_A));
    a.pha();
    a.lda(zp(z::TEXT_A));
    a.pha();
    a.lda(zp(z::TEXT_A + 1));
    a.pha();
    a.lda(zp(ITEM));
    a.sta(zp(z::TEXT_A));
    a.lda(zp(ITEM + 1));
    a.sta(zp(z::TEXT_A + 1));
    a.sty(zp(z::OFFSET_A));
    a.jsr("expr");
    a.sta(zp(z::TYPE));
    a.ldy(zp(z::OFFSET_A));
    a.dey();
    a.lda(zp(z::TEXT_A));
    a.sta(zp(ITEM));
    a.lda(zp(z::TEXT_A + 1));
    a.sta(zp(ITEM + 1));
    a.jsr("data_past");
    a.pla();
    a.sta(zp(z::TEXT_A + 1));
    a.pla();
    a.sta(zp(z::TEXT_A));
    a.pla();
    a.sta(zp(z::OFFSET_A));
    a.lda(zp(z::TYPE));
    a.jmp(stored);
    a.bind(string);
    a.jsr("raw_item");
    a.jsr("data_past");
    a.lda(imm(kind::STRING));
    a.bind(stored);
    a.tax();
    a.pla();
    a.sta(zp(z::VTYPE));
    a.pla();
    a.sta(zp(z::VAR + 1));
    a.pla();
    a.sta(zp(z::VAR));
    a.txa();
    a.jsr("store_variable");
    a.jsr("next_a");
    a.cmp(imm(b','));
    a.beq("statement_read");
    a.tax();
    a.jmp("statement_done_x");
    a.bind(mistake);
    a.jmp("err_mistake");
}

fn data(a: &mut Asm) {
    // The DATA pointer at (ITEM),Y, the comma or CR an item ended at.
    a.here("data_past");
    a.tya();
    a.clc();
    a.adc(zp(ITEM));
    a.sta(zp(z::DATA));
    a.lda(zp(ITEM + 1));
    a.adc(imm(0));
    a.sta(zp(z::DATA + 1));
    a.rts();

    // The next DATA item: after the comma the pointer is at, or in the next
    // line from the pointer's that holds DATA. (ITEM),Y its start.
    a.here("data_item");
    a.lda(zp(z::DATA));
    a.sta(zp(ITEM));
    a.lda(zp(z::DATA + 1));
    a.sta(zp(ITEM + 1));
    a.ldy(imm(0));
    a.lda(zp(ITEM).ind_y());
    a.cmp(imm(b','));
    let (record, found) = (a.local(), a.local());
    a.bne(record);
    a.iny();
    a.rts();
    // A record from its CR: Out of DATA at the program's end, else its
    // DATA token's next character, else the next record.
    a.bind(record);
    a.ldy(imm(1));
    a.lda(zp(ITEM).ind_y());
    let more = a.local();
    a.bpl(more);
    a.jmp("err_out_of_data");
    a.bind(more);
    a.ldy(imm(3));
    let scan = a.here_local();
    a.iny();
    a.lda(zp(ITEM).ind_y());
    a.cmp(imm(DATA_TOKEN));
    a.beq(found);
    a.cmp(imm(CR));
    a.bne(scan);
    a.tya();
    a.clc();
    a.adc(zp(ITEM));
    a.sta(zp(ITEM));
    a.bcc(record);
    a.inc(zp(ITEM + 1));
    a.bne(record);
    a.bind(found);
    a.iny();
    a.rts();
}

fn restoring(a: &mut Asm) {
    // RESTORE [line]: the DATA pointer to the program's start or the
    // line's record.
    a.here("statement_restore");
    a.jsr("next_a");
    a.dec(zp(z::OFFSET_A));
    a.jsr("ends_statement");
    let at_line = a.local();
    a.bne(at_line);
    a.jsr("restore_data");
    a.jmp("statement_done");
    a.bind(at_line);
    a.jsr("line_target");
    a.jsr("find_line");
    let line = a.local();
    a.beq(line);
    a.jmp("err_no_such_line");
    a.bind(line);
    a.lda(zp(super::editor::RECORD));
    a.sta(zp(z::DATA));
    a.lda(zp(super::editor::RECORD + 1));
    a.sta(zp(z::DATA + 1));
    a.jmp("statement_done");
}

fn input(a: &mut Asm) -> Label {
    // INPUT [LINE] ["prompt"[,;']] v[, ...]: the line read on the BASIC
    // stack, its items to the variables; another line asked for when one
    // runs out.
    a.here("statement_input");
    a.jsr("next_a");
    a.dec(zp(z::OFFSET_A));
    a.cmp(imm(b'#'));
    let keyboard = a.local();
    a.bne(keyboard);
    a.jmp("input_file");
    a.bind(keyboard);
    a.lda(imm(LINE_ROOM));
    a.sec();
    a.jsr("stack_claim");
    a.ldx(imm(0));
    a.jsr("next_a");
    a.cmp(imm(LINE_TOKEN));
    let whole = a.local();
    a.beq(whole);
    a.dec(zp(z::OFFSET_A));
    a.inx();
    a.bind(whole);
    a.dex();
    a.stx(zp(WHOLE));
    a.lda(imm(0xFF));
    a.sta(zp(ASK));
    a.sta(zp(NEXT_ITEM));
    let (next, finished) = (a.here_local(), a.local());
    a.jsr("next_a");
    let (prompt, separator, newline, variable) = (a.local(), a.local(), a.local(), a.local());
    a.cmp(imm(b'"'));
    a.beq(prompt);
    a.cmp(imm(b','));
    a.beq(separator);
    a.cmp(imm(b';'));
    a.beq(separator);
    a.cmp(imm(b'\''));
    a.beq(newline);
    a.jsr("ends_statement");
    a.beq(finished);
    a.jmp(variable);
    a.bind(prompt);
    a.jsr("factor_string");
    a.jsr("print_buffer");
    a.dec(zp(z::OFFSET_A));
    a.lda(imm(0));
    a.sta(zp(ASK));
    a.beq(next);
    a.bind(separator);
    a.lda(imm(0xFF));
    a.sta(zp(ASK));
    a.bne(next);
    a.bind(newline);
    a.jsr("print_newline");
    a.jmp(next);
    a.bind(finished);
    a.tax();
    a.lda(imm(LINE_ROOM));
    a.jsr("stack_release");
    a.jmp("statement_done_x");
    a.bind(variable);
    a.dec(zp(z::OFFSET_A));
    a.jsr("lvalue");
    let located = a.local();
    a.bcc(located);
    a.jmp("err_mistake");
    a.bind(located);
    a.lda(zp(z::VAR));
    a.pha();
    a.lda(zp(z::VAR + 1));
    a.pha();
    a.lda(zp(z::VTYPE));
    a.pha();
    next
}

fn input_items(a: &mut Asm, next: Label) {
    // A line read when none is left, `?` asked unless a prompt said not.
    a.lda(zp(z::STACK));
    a.sta(zp(ITEM));
    a.lda(zp(z::STACK + 1));
    a.sta(zp(ITEM + 1));
    a.ldy(zp(NEXT_ITEM));
    let have = a.local();
    a.bit(zp(WHOLE));
    a.bmi("input_read");
    a.cpy(imm(0xFF));
    a.bne(have);
    a.here("input_read");
    a.bit(zp(ASK));
    let quiet = a.local();
    a.bpl(quiet);
    a.lda(imm(b'?'));
    a.jsr("print_char");
    a.bind(quiet);
    a.lda(zp(z::STACK));
    a.sta(zp(LINE_BLOCK));
    a.lda(zp(z::STACK + 1));
    a.sta(zp(LINE_BLOCK + 1));
    a.lda(imm(LINE_ROOM - 2));
    a.sta(zp(LINE_BLOCK + 2));
    a.lda(imm(b' '));
    a.sta(zp(LINE_BLOCK + 3));
    a.lda(imm(0xFF));
    a.sta(zp(LINE_BLOCK + 4));
    a.sta(zp(ASK));
    a.ldx(imm(lo(LINE_BLOCK)));
    a.ldy(imm(hi(LINE_BLOCK)));
    a.lda(imm(0));
    a.jsr(entry::OSWORD);
    let read = a.local();
    a.bcc(read);
    a.jmp("escaped");
    a.bind(read);
    a.lda(imm(0));
    a.sta(zp(z::COUNT));
    a.tay();
    a.bind(have);
    // The item: the whole line for LINE, else up to a comma.
    a.bit(zp(WHOLE));
    let (part, taken) = (a.local(), a.local());
    a.bpl(part);
    a.ldx(imm(0));
    let copy = a.here_local();
    a.lda(zp(ITEM).ind_y());
    a.cmp(imm(CR));
    let copied = a.local();
    a.beq(copied);
    a.sta(abs(page::STRING).x());
    a.iny();
    a.inx();
    a.bne(copy);
    a.bind(copied);
    a.stx(zp(z::STRING_LENGTH));
    a.jmp(taken);
    a.bind(part);
    a.jsr("raw_item");
    a.bind(taken);
    // The line is used up at its CR; past a comma the next item follows.
    a.lda(zp(ITEM).ind_y());
    a.cmp(imm(CR));
    let rest = a.local();
    a.bne(rest);
    a.ldy(imm(0xFE));
    a.bind(rest);
    a.iny();
    a.sty(zp(NEXT_ITEM));
    a.pla();
    a.sta(zp(z::VTYPE));
    a.pla();
    a.sta(zp(z::VAR + 1));
    a.pla();
    a.sta(zp(z::VAR));
    a.lda(zp(z::VTYPE));
    let string = a.local();
    a.bmi(string);
    a.jsr("val_buffer");
    a.jmp("input_store");
    a.bind(string);
    a.lda(imm(kind::STRING));
    a.here("input_store");
    a.jsr("store_variable");
    a.jmp(next);
}

fn printing(a: &mut Asm) -> Label {
    // PRINT: each item right in its field unless after `;`, `~` making
    // the next numbers hex until a separator, `,` to the next field, `'` a
    // new line, and a new line at the end unless it ends with `;`.
    a.here("statement_print");
    a.jsr("next_a");
    a.dec(zp(z::OFFSET_A));
    a.cmp(imm(b'#'));
    let screen = a.local();
    a.bne(screen);
    a.jmp("print_file");
    a.bind(screen);
    a.lda(abs(AT_WIDTH));
    a.sta(zp(z::FIELD));
    a.lda(imm(0));
    a.sta(zp(z::HEX));
    a.here("print_item");
    a.lda(imm(0));
    a.sta(zp(GOES_ON));
    a.here("print_next");
    a.jsr("next_a");
    let (ended, semicolon, comma, quote, tilde, tab, spc) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    for (character, target) in [
        (b':', ended),
        (CR, ended),
        (ELSE, ended),
        (b';', semicolon),
        (b',', comma),
        (b'\'', quote),
        (b'~', tilde),
        (SPC, spc),
    ] {
        a.cmp(imm(character));
        a.beq(target);
    }
    a.cmp(imm(TAB));
    let item = a.local();
    a.bne(item);
    a.jmp(tab);
    a.bind(item);
    a.dec(zp(z::OFFSET_A));
    a.jsr("expr");
    a.dec(zp(z::OFFSET_A));
    a.jsr("print_value");
    a.jmp("print_item");
    a.bind(ended);
    a.tax();
    a.bit(zp(GOES_ON));
    let done = a.local();
    a.bmi(done);
    a.txa();
    a.pha();
    a.jsr("print_newline");
    a.pla();
    a.tax();
    a.bind(done);
    a.jmp("statement_done_x");
    a.bind(semicolon);
    a.lda(imm(0));
    a.sta(zp(z::FIELD));
    a.sta(zp(z::HEX));
    a.lda(imm(0xFF));
    a.sta(zp(GOES_ON));
    a.bne("print_next");
    a.bind(comma);
    a.jsr("print_field");
    a.lda(abs(AT_WIDTH));
    a.sta(zp(z::FIELD));
    a.lda(imm(0));
    a.sta(zp(z::HEX));
    a.jmp("print_item");
    a.bind(quote);
    a.jsr("print_newline");
    a.jmp("print_item");
    a.bind(tilde);
    a.lda(imm(0xFF));
    a.sta(zp(z::HEX));
    a.jmp("print_item");
    a.bind(spc);
    a.jsr("factor_integer");
    a.dec(zp(z::OFFSET_A));
    a.ldx(zp(z::IAC));
    a.jsr("print_spaces");
    a.jmp("print_item");
    tab
}

fn print_tab(a: &mut Asm, tab: Label) {
    // TAB(x): to column x, on a new line if past it; TAB(x,y) moves the
    // cursor.
    a.bind(tab);
    a.jsr("expr_integer");
    a.cpx(imm(b','));
    let column = a.local();
    a.bne(column);
    a.lda(zp(z::IAC));
    a.pha();
    a.jsr("expr_integer");
    a.jsr("closing_bracket");
    a.lda(imm(31));
    a.jsr(entry::OSWRCH);
    a.pla();
    a.jsr(entry::OSWRCH);
    a.lda(zp(z::IAC));
    a.jsr(entry::OSWRCH);
    a.jmp("print_item");
    a.bind(column);
    a.jsr("closing_bracket");
    a.lda(zp(z::IAC));
    a.sec();
    a.sbc(zp(z::COUNT));
    let reached = a.local();
    a.bcs(reached);
    a.jsr("print_newline");
    a.lda(zp(z::IAC));
    a.bind(reached);
    a.tax();
    a.jsr("print_spaces");
    a.jmp("print_item");

    // X the character after an expression: Missing ) unless a `)`.
    a.here("closing_bracket");
    a.cpx(imm(b')'));
    a.beq(RTS);
    a.jmp("err_missing_bracket");

    // Spaces to the next multiple of the `@%` width.
    a.here("print_field");
    a.lda(abs(AT_WIDTH));
    a.beq(RTS);
    a.sta(zp(z::FIELD));
    a.lda(zp(z::COUNT));
    a.sec();
    let modulo = a.here_local();
    a.sbc(zp(z::FIELD));
    a.bcs(modulo);
    a.eor(imm(0xFF));
    a.adc(imm(1));
    a.tax();
    a.cpx(zp(z::FIELD));
    a.bne("print_spaces");
    a.rts();
}

fn output(a: &mut Asm) {
    // X spaces.
    a.here("print_spaces");
    a.txa();
    a.beq(RTS);
    a.jsr("print_space");
    a.dex();
    a.jmp("print_spaces");

    // A new line, COUNT back to 0.
    a.here("print_newline");
    a.jsr(entry::OSNEWL);
    a.lda(imm(0));
    a.sta(zp(z::COUNT));
    a.rts();

    a.here("print_space");
    a.lda(imm(b' '));
    a.fall_into("print_char");

    // A printed and counted, a new line first once WIDTH is filled; a CR
    // starts the count again. X and Y kept.
    a.here("print_char");
    a.pha();
    a.lda(zp(z::WIDTH));
    a.cmp(zp(z::COUNT));
    let room = a.local();
    a.bcs(room);
    a.jsr("print_newline");
    a.bind(room);
    a.pla();
    a.inc(zp(z::COUNT));
    a.cmp(imm(CR));
    let printed = a.local();
    a.bne(printed);
    a.pha();
    a.lda(imm(0));
    a.sta(zp(z::COUNT));
    a.pla();
    a.bind(printed);
    a.jmp(abs(entry::OSWRCH));

    // The string buffer printed.
    a.here("print_buffer");
    a.ldy(imm(0));
    let character = a.here_local();
    a.cpy(zp(z::STRING_LENGTH));
    a.beq(RTS);
    a.lda(abs(page::STRING).y());
    a.jsr("print_char");
    a.iny();
    a.bne(character);
    a.rts();
}

fn numbers(a: &mut Asm) {
    // The value of kind A printed: a string as it is, a number in hex or by
    // `@%`, right in the field.
    a.here("print_value");
    a.tay();
    a.beq("print_buffer");
    a.bit(zp(z::HEX));
    let decimal = a.local();
    a.bpl(decimal);
    a.jsr("make_integer");
    a.jsr("hex_string");
    a.jmp("print_justified");
    a.bind(decimal);
    a.jsr("make_real");
    a.jsr("format_at");
    a.fall_into("print_justified");

    // The buffer printed right in FIELD.
    a.here("print_justified");
    a.lda(zp(z::FIELD));
    a.sec();
    a.sbc(zp(z::STRING_LENGTH));
    let full = a.local();
    a.bcc(full);
    a.tax();
    a.jsr("print_spaces");
    a.bind(full);
    a.jmp("print_buffer");

    // FAC written in the buffer by `@%`.
    a.here("format_at");
    a.lda(abs(AT_FORMAT));
    a.sta(zp(FORMAT));
    a.lda(abs(AT_DIGITS));
    a.sta(zp(PLACES));
    a.jmp("format_number");

    // FAC written in the buffer as STR$ writes it: by `@%` when its top
    // byte is set, otherwise the general form with the most digits.
    a.here("format_str");
    a.lda(abs(AT_STR));
    a.bne("format_at");
    a.sta(zp(FORMAT));
    a.sta(zp(PLACES));
    a.jmp("format_number");

    // IAC in hex in the buffer, without leading zeros.
    a.here("hex_string");
    a.lda(imm(0));
    a.sta(zp(z::STRING_LENGTH));
    a.ldx(imm(3));
    let byte = a.here_local();
    a.lda(zp(z::IAC).x());
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.jsr("hex_digit");
    a.lda(zp(z::IAC).x());
    a.and(imm(0x0F));
    a.jsr("hex_digit");
    a.dex();
    a.bpl(byte);
    a.lda(zp(z::STRING_LENGTH));
    let some = a.local();
    a.bne(some);
    a.jsr("hex_put");
    a.bind(some);
    a.lda(imm(kind::STRING));
    a.rts();

    // A's digit appended, unless a leading zero.
    a.here("hex_digit");
    a.ldy(zp(z::STRING_LENGTH));
    a.bne("hex_put");
    a.cmp(imm(0));
    a.beq(RTS);
    a.here("hex_put");
    a.cmp(imm(10));
    let digit = a.local();
    a.bcc(digit);
    a.adc(imm(b'A' - b'0' - 10 - 1));
    a.bind(digit);
    a.adc(imm(b'0'));
    a.ldy(zp(z::STRING_LENGTH));
    a.sta(abs(page::STRING).y());
    a.inc(zp(z::STRING_LENGTH));
    a.rts();
}
