//! The program and the prompt: starting, reading and tokenising each typed
//! line, keeping a numbered one in the program in its order, and NEW, OLD
//! and LIST.
//!
//! A program is records from PAGE, each a CR, the line number high byte
//! first, the record's length and its text; a CR and &FF end it, TOP just
//! past them.

use super::crunch::{FROM, NUMBER, REFERENCE, WRITE};
use super::list::LINE;
use super::tokens::LINE_REFERENCE;
use super::ws::{page, zp as z, AT_DEFAULT};
use super::FAILED_AT;
use super::SAVED_EXEC;
use crate::rom::asm::{abs, hi, imm, lo, zp, Asm, Label, RTS};
use crate::rom::os::{entry, osbyte, ESCAPE_FLAG};

/// The record the editor works at; a move's or a walk's pointer, and the
/// number a walked record is given; a record's length.
pub(super) const RECORD: u16 = z::SCRATCH + 21;
const SOURCE: u16 = RECORD + 2;
const WALKED_NUMBER: u16 = SOURCE + 2;
const LENGTH: u16 = WALKED_NUMBER + 2;
/// The number of the line being entered or looked for.
pub(super) const LINE_NUMBER: u16 = LENGTH + 2;

/// The longest line OSWORD 0 reads, and the characters it takes.
const LINE_LIMIT: u8 = 238;
/// Where AUTO reads a line's text: after where its number's reference
/// would be, so the record is made as for a typed line.
const AUTO_TEXT: u16 = page::INPUT + HEADER as u16;
/// LIST's first and last lines and DELETE's last; AUTO's and RENUMBER's
/// step, and while renumbering the number given and a reference's target.
const FIRST: u16 = z::STATEMENT;
const LAST: u16 = FIRST + 2;
const STEP: u16 = LAST + 2;
const GIVEN: u16 = STEP + 1;
const TARGET: u16 = GIVEN + 2;
/// A listing's open loops, FOR's then REPEAT's.
pub(super) const LISTING_LOOPS: u16 = TARGET + 2;
/// The step a number is given without one.
const DEFAULT_STEP: u8 = 10;
/// OSFILE's block, and its reasons.
const FILE_BLOCK: u16 = z::SCRATCH;
const FILE_BLOCK_LENGTH: u8 = 18;
const OSFILE_SAVE: u8 = 0;
const OSFILE_LOAD: u8 = 0xFF;
const CR: u8 = 0x0D;
/// The program's end: a record whose line number's high byte is this.
const END: u8 = 0xFF;
/// The highest line number, the one a line number's top bit clear allows.
const LAST_LINE: u16 = 32_767;
const _: () = assert!(LAST_LINE == 0x7FFF);
/// `@%` starts with its top two bytes 0; SAVE's reason is also the 0 of its
/// start's low byte, as PAGE's; and a file's name is in a buffer at a page's
/// start.
const _: () =
    assert!(AT_DEFAULT >> 16 == 0 && OSFILE_SAVE == 0 && page::STRING.trailing_zeros() >= 8);
/// A record's bytes before its text, as many as an encoded line number's.
const HEADER: u8 = REFERENCE;
/// The seed's first bytes as BASIC II starts RND from an all-zero seed, as
/// at power-on; the rest stay zero.
const FIRST_SEED: [u8; 3] = *b"ARW";

pub fn emit(a: &mut Asm) {
    starting(a);
    prompt(a);
    entering(a);
    program(a);
    clearing(a);
    moves(a);
    let syntax = listing(a);
    deleting(a, syntax);
    let silly = numbering(a);
    renumbering(a, silly);
    records(a);
    files(a);
}

fn starting(a: &mut Asm) {
    // Every entry: HIMEM and PAGE from the MOS, `@%` and the listing and
    // trace options as BASIC II starts, and an empty program.
    a.here("cold");
    a.lda(imm(osbyte::HIMEM));
    a.jsr(entry::OSBYTE);
    a.stx(zp(z::HIMEM));
    a.sty(zp(z::HIMEM + 1));
    a.lda(imm(osbyte::OSHWM));
    a.jsr(entry::OSBYTE);
    a.sty(zp(z::PAGE));
    let [width, digits, ..] = AT_DEFAULT.to_le_bytes();
    a.lda(imm(width));
    a.sta(abs(page::RESIDENT));
    a.lda(imm(digits));
    a.sta(abs(page::RESIDENT + 1));
    a.lda(imm(0));
    a.sta(abs(page::RESIDENT + 2));
    a.sta(abs(page::RESIDENT + 3));
    a.sta(zp(z::LISTO));
    a.sta(zp(z::TRACE));
    a.lda(imm(0xFF));
    a.sta(zp(z::WIDTH));
    // RND's seed, unless one survives from before BREAK.
    a.lda(zp(z::SEED));
    for at in z::SEED + 1..z::SEED + z::SEED_LEN {
        a.ora(zp(at));
    }
    let seeded = a.local();
    a.bne(seeded);
    for (at, byte) in (z::SEED..).zip(FIRST_SEED) {
        a.lda(imm(byte));
        a.sta(zp(at));
    }
    a.bind(seeded);
    a.fall_into("statement_new");

    // NEW: a CR at PAGE and the end after it, so OLD can take it back.
    a.here("statement_new");
    a.ldx(imm(lo(RECORD)));
    a.jsr("page_pointer");
    a.tay();
    a.lda(imm(CR));
    a.sta(zp(RECORD).ind_y());
    a.lda(imm(END));
    let mark = a.local();
    a.bne(mark);
    // OLD: the first record's number restored.
    a.here("statement_old");
    a.ldx(imm(lo(RECORD)));
    a.jsr("page_pointer");
    a.bind(mark);
    a.ldy(imm(1));
    a.sta(zp(RECORD).ind_y());
    // TOP found and the variables cleared, then the prompt.
    a.here("prompt_fresh");
    a.jsr("fresh_program");
    a.fall_into("prompt");
}

fn prompt(a: &mut Asm) {
    // `>`, a line read and tokenised; a numbered one into the program, any
    // other run at once.
    a.here("prompt");
    a.ldx(imm(0xFF));
    a.txs();
    a.jsr("clear_stacks");
    // From here an error is in a typed line, not the program.
    a.lda(imm(0));
    a.sta(zp(z::OFFSET_A));
    a.sta(zp(z::TEXT_A));
    a.lda(imm(hi(page::INPUT)));
    a.sta(zp(z::TEXT_A + 1));
    a.lda(imm(b'>'));
    a.ldx(imm(lo("line_block")));
    a.ldy(imm(hi("line_block")));
    a.jsr("read_line");
    a.here("prompt_spaces");
    a.lda(abs(page::INPUT).y());
    a.cmp(imm(b' '));
    let started = a.local();
    a.bne(started);
    a.iny();
    a.bne("prompt_spaces");
    a.bind(started);
    a.jsr("crunch");
    a.lda(abs(page::INPUT));
    a.cmp(imm(LINE_REFERENCE));
    a.beq("enter_line");
    // A command only as the line's first keyword.
    a.jsr("next_a");
    a.sec();
    a.sbc(imm(super::exec::FIRST_COMMAND));
    let statement = a.local();
    a.bcc(statement);
    a.cmp(imm(super::byte(super::exec::COMMANDS.len())));
    a.bcs(statement);
    a.tax();
    a.lda(abs("command_high").x());
    a.pha();
    a.lda(abs("command_low").x());
    a.pha();
    a.rts();
    a.bind(statement);
    a.dec(zp(z::OFFSET_A));
    a.jmp("statement");

    // After an error and END: the program checked as TOP is found again.
    a.here("prompt_checked");
    a.jsr("find_top");
    a.jmp("prompt");
}

fn entering(a: &mut Asm) {
    // A numbered line, X long tokenised with its number's reference first,
    // the number at most 32767.
    a.here("enter_line");
    a.stx(zp(LENGTH));
    a.jsr("line_argument");
    a.tya();
    let numbered = a.local();
    a.bpl(numbered);
    a.jmp("err_syntax");
    a.bind(numbered);
    a.sta(zp(LINE_NUMBER + 1));
    a.stx(zp(LINE_NUMBER));
    a.jsr("store_line");
    a.jmp("prompt");

    // Line LINE_NUMBER made the LENGTH-byte record whose text is the input
    // buffer's from its fifth byte, any old line of that number gone; four
    // bytes long it has no text and is only deleted. The variables go.
    a.here("store_line");
    a.jsr("find_line");
    let insert = a.local();
    a.bne(insert);
    a.jsr("delete_record");
    a.bind(insert);
    a.lda(zp(LENGTH));
    a.cmp(imm(HEADER));
    let done = a.local();
    a.beq(done);
    a.jsr("open_record");
    a.ldy(imm(0));
    a.lda(imm(CR));
    a.sta(zp(RECORD).ind_y());
    a.iny();
    a.lda(zp(LINE_NUMBER + 1));
    a.sta(zp(RECORD).ind_y());
    a.iny();
    a.lda(zp(LINE_NUMBER));
    a.sta(zp(RECORD).ind_y());
    a.iny();
    a.lda(zp(LENGTH));
    a.sta(zp(RECORD).ind_y());
    let text = a.here_local();
    a.iny();
    a.cpy(zp(LENGTH));
    a.beq(done);
    a.lda(abs(page::INPUT).y());
    a.sta(zp(RECORD).ind_y());
    a.jmp(text);
    a.bind(done);
    a.jmp("clear_variables");
}

fn program(a: &mut Asm) {
    // TOP found, then the variables cleared.
    a.here("fresh_program");
    a.jsr("find_top");
    a.jmp("clear_variables");

    // TOP from the records at PAGE, each stepped over by its length until
    // the end marker, and checked to start with a CR and to end below
    // HIMEM. A record that does not leaves TOP at it and, as BASIC II does,
    // reports Bad program as a message, ERR untouched, and returns to the
    // prompt; a length of zero, which would never step on, is one.
    a.here("find_top");
    a.ldx(imm(lo(z::TOP)));
    a.jsr("page_pointer");
    let (record, bad, end) = (a.here_local(), a.local(), a.local());
    a.ldy(imm(0));
    a.lda(zp(z::TOP).ind_y());
    a.cmp(imm(CR));
    a.bne(bad);
    a.iny();
    a.lda(zp(z::TOP).ind_y());
    a.bmi(end);
    a.ldy(imm(3));
    a.lda(zp(z::TOP).ind_y());
    a.beq(bad);
    a.jsr("pointer_add");
    a.lda(zp(z::TOP + 1));
    a.cmp(zp(z::HIMEM + 1));
    a.bcc(record);
    a.bind(bad);
    a.jsr("print_newline");
    a.ldx(imm(super::BAD_PROGRAM));
    a.jsr("print_message");
    a.jsr(entry::OSNEWL);
    a.jmp("prompt");
    a.bind(end);
    a.lda(imm(2));
    a.fall_into("pointer_add");

    // A added to the pointer at zero page X; C clear after.
    a.here("pointer_add");
    a.clc();
    a.adc(zp(0).x());
    a.sta(zp(0).x());
    a.bcc(RTS);
    a.inc(zp(1).x());
    a.clc();
    a.rts();

    // The pointer at zero page X at PAGE; A 0.
    a.here("page_pointer");
    a.lda(zp(z::PAGE));
    a.sta(zp(1).x());
    a.lda(imm(0));
    a.sta(zp(0).x());
    a.rts();
}

fn clearing(a: &mut Asm) {
    // The variables gone: LOMEM and the heap at TOP, every list empty.
    a.here("clear_variables");
    a.lda(zp(z::TOP));
    a.sta(zp(z::LOMEM));
    a.lda(zp(z::TOP + 1));
    a.sta(zp(z::LOMEM + 1));
    a.here("clear_lists");
    a.lda(zp(z::LOMEM));
    a.sta(zp(z::VARTOP));
    a.lda(zp(z::LOMEM + 1));
    a.sta(zp(z::VARTOP + 1));
    a.lda(imm(0));
    a.ldx(imm(0x7F));
    let list = a.here_local();
    a.sta(abs(page::CATALOGUE).x());
    a.dex();
    a.bpl(list);
    a.fall_into("clear_stacks");

    // No FOR, REPEAT or GOSUB open, the BASIC stack empty, and READ from
    // the program's first DATA again, as each typed line and each clearing
    // starts it in BASIC II.
    a.here("clear_stacks");
    a.lda(imm(0));
    a.sta(zp(z::REPEATS));
    a.sta(zp(z::GOSUBS));
    a.sta(zp(z::FORS));
    a.lda(zp(z::HIMEM));
    a.sta(zp(z::STACK));
    a.lda(zp(z::HIMEM + 1));
    a.sta(zp(z::STACK + 1));
    a.fall_into("restore_data");

    // The DATA pointer at the program's start.
    a.here("restore_data");
    a.ldx(imm(lo(z::DATA)));
    a.jmp("page_pointer");

    // The record of the first line numbered at least LINE_NUMBER, or the
    // program's end: Z set if it is that line.
    a.here("find_line");
    a.lda(imm(0));
    a.sta(zp(RECORD));
    a.lda(zp(z::PAGE));
    a.sta(zp(RECORD + 1));
    a.here("find_line_record");
    a.ldy(imm(1));
    a.lda(zp(RECORD).ind_y());
    a.cmp(zp(LINE_NUMBER + 1));
    a.bne("find_line_high");
    a.iny();
    a.lda(zp(RECORD).ind_y());
    a.cmp(zp(LINE_NUMBER));
    a.beq(RTS);
    a.here("find_line_high");
    a.bcs("find_line_past");
    a.ldy(imm(3));
    a.lda(zp(RECORD).ind_y());
    a.clc();
    a.adc(zp(RECORD));
    a.sta(zp(RECORD));
    a.bcc("find_line_record");
    a.inc(zp(RECORD + 1));
    a.bne("find_line_record");
    a.here("find_line_past");
    a.lda(imm(1));
    a.rts();

    // The number of the line holding the statement pointer's last
    // character read, in XY; 0 when the line is typed.
    a.here("current_line");
    a.ldx(imm(0));
    a.ldy(imm(0));
    a.lda(zp(z::TEXT_A + 1));
    a.cmp(imm(hi(page::INPUT)));
    a.beq(RTS);
    // RECORD from PAGE on while the next record starts before the pointer
    // less one.
    a.jsr("return_point");
    a.stx(zp(SOURCE));
    a.sty(zp(SOURCE + 1));
    a.ldx(imm(lo(RECORD)));
    a.jsr("page_pointer");
    let (record, here) = (a.here_local(), a.local());
    a.ldy(imm(1));
    a.lda(zp(RECORD).ind_y());
    a.bmi(here);
    a.ldy(imm(3));
    a.lda(zp(RECORD).ind_y());
    a.sec();
    a.adc(zp(RECORD));
    a.tax();
    a.lda(zp(RECORD + 1));
    a.adc(imm(0));
    a.cpx(zp(SOURCE));
    a.sbc(zp(SOURCE + 1));
    a.bcs(here);
    a.ldy(imm(3));
    a.lda(zp(RECORD).ind_y());
    a.ldx(imm(lo(RECORD)));
    a.jsr("pointer_add");
    a.bcc(record);
    a.bind(here);
    a.ldy(imm(2));
    a.lda(zp(RECORD).ind_y());
    a.tax();
    a.dey();
    a.lda(zp(RECORD).ind_y());
    a.tay();
    a.rts();
}

/// Opening and closing a record's room in the program.
fn moves(a: &mut Asm) {
    // The record at RECORD taken out: the program above it moved down,
    // TOP with it, which also marks where the moving ends.
    a.here("delete_record");
    a.ldy(imm(3));
    a.lda(zp(RECORD).ind_y());
    a.tax();
    a.lda(zp(RECORD));
    a.sta(zp(SOURCE));
    a.lda(zp(RECORD + 1));
    a.sta(zp(SOURCE + 1));
    a.txa();
    a.eor(imm(0xFF));
    a.sec();
    a.adc(zp(z::TOP));
    a.sta(zp(z::TOP));
    let lowered = a.local();
    a.bcs(lowered);
    a.dec(zp(z::TOP + 1));
    a.bind(lowered);
    // Each byte X above SOURCE moved to SOURCE, until SOURCE reaches TOP.
    let byte = a.here_local();
    a.lda(zp(SOURCE));
    a.cmp(zp(z::TOP));
    a.lda(zp(SOURCE + 1));
    a.sbc(zp(z::TOP + 1));
    a.bcs(RTS);
    a.txa();
    a.tay();
    a.lda(zp(SOURCE).ind_y());
    a.ldy(imm(0));
    a.sta(zp(SOURCE).ind_y());
    a.inc(zp(SOURCE));
    a.bne(byte);
    a.inc(zp(SOURCE + 1));
    a.bne(byte);

    // LENGTH bytes of room at RECORD, the program above moved up and TOP
    // with it; No room if that reaches HIMEM.
    a.here("open_record");
    a.lda(zp(z::TOP));
    a.sta(zp(SOURCE));
    a.clc();
    a.adc(zp(LENGTH));
    a.tax();
    a.lda(zp(z::TOP + 1));
    a.sta(zp(SOURCE + 1));
    a.adc(imm(0));
    a.tay();
    a.cpx(zp(z::HIMEM));
    a.sbc(zp(z::HIMEM + 1));
    let room = a.local();
    a.bcc(room);
    a.jmp("err_no_room");
    a.bind(room);
    a.stx(zp(z::TOP));
    a.sty(zp(z::TOP + 1));
    // Down to RECORD, the byte below SOURCE moved up LENGTH.
    let (byte, step) = (a.here_local(), a.local());
    a.lda(zp(SOURCE));
    a.cmp(zp(RECORD));
    a.bne(step);
    a.lda(zp(SOURCE + 1));
    a.cmp(zp(RECORD + 1));
    a.beq(RTS);
    a.bind(step);
    a.lda(zp(SOURCE));
    let same = a.local();
    a.bne(same);
    a.dec(zp(SOURCE + 1));
    a.bind(same);
    a.dec(zp(SOURCE));
    a.ldy(imm(0));
    a.lda(zp(SOURCE).ind_y());
    a.ldy(zp(LENGTH));
    a.sta(zp(SOURCE).ind_y());
    a.jmp(byte);
}

/// LIST's Syntax error, which DELETE shares.
fn listing(a: &mut Asm) -> Label {
    // A printed, then a line read with the OSWORD 0 block at XY; COUNT and
    // Y 0 after.
    a.here("read_line");
    a.jsr("print_char");
    a.lda(imm(0));
    a.jsr(entry::OSWORD);
    a.bcs("escaped");
    a.ldy(imm(0));
    a.sty(zp(z::COUNT));
    a.rts();

    // Escape while a line is read or listed.
    a.here("escaped");
    a.lda(imm(osbyte::ACKNOWLEDGE_ESCAPE));
    a.jsr(entry::OSBYTE);
    a.jmp("err_escape");

    a.here("line_block");
    a.word(page::INPUT);
    a.byte(LINE_LIMIT);
    a.byte(b' ');
    a.byte(0xFF);

    // LAST the line number next, else &FFFF; C as line_argument leaves it.
    a.here("last_argument");
    a.lda(imm(0xFF));
    a.sta(zp(LAST));
    a.sta(zp(LAST + 1));
    a.jsr("line_argument");
    a.bcs(RTS);
    a.stx(zp(LAST));
    a.sty(zp(LAST + 1));
    a.rts();

    // LIST, with any first and last lines: a line alone, a range, or a
    // range open at either end; its lines listed, Escape stopping them.
    // LISTO: its options set. Then the prompt.
    a.here("statement_list");
    a.ldy(zp(z::OFFSET_A));
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'O'));
    let listo = a.local();
    a.beq(listo);
    a.jsr("find_top");
    a.lda(imm(0));
    a.sta(zp(FIRST));
    a.sta(zp(FIRST + 1));
    a.jsr("last_argument");
    let ranged = a.local();
    a.bcs(ranged);
    a.stx(zp(FIRST));
    a.sty(zp(FIRST + 1));
    a.bind(ranged);
    a.jsr("next_a");
    a.cmp(imm(b','));
    let ended = a.local();
    a.bne(ended);
    a.jsr("last_argument");
    a.jsr("next_a");
    a.bind(ended);
    let (listed, syntax) = (a.local(), a.local());
    a.cmp(imm(b':'));
    a.beq(listed);
    a.cmp(imm(CR));
    a.beq(listed);
    a.bind(syntax);
    a.jmp("err_syntax");
    a.bind(listed);
    a.ldx(imm(lo(LINE)));
    a.jsr("page_pointer");
    a.sta(zp(LISTING_LOOPS));
    a.sta(zp(LISTING_LOOPS + 1));
    let (next, done) = (a.here_local(), a.local());
    a.bit(zp(ESCAPE_FLAG));
    a.bmi("escaped");
    a.ldy(imm(1));
    a.lda(zp(LINE).ind_y());
    a.cmp(imm(END));
    a.beq(done);
    // Past LAST the listing ends; before FIRST a line is passed over.
    a.ldy(imm(2));
    a.lda(zp(LAST));
    a.cmp(zp(LINE).ind_y());
    a.dey();
    a.lda(zp(LAST + 1));
    a.sbc(zp(LINE).ind_y());
    a.bcc(done);
    a.iny();
    a.lda(zp(LINE).ind_y());
    a.cmp(zp(FIRST));
    a.dey();
    a.lda(zp(LINE).ind_y());
    a.sbc(zp(FIRST + 1));
    let before = a.local();
    a.bcc(before);
    a.jsr("list_line");
    a.bind(before);
    a.ldy(imm(3));
    a.lda(zp(LINE).ind_y());
    a.ldx(imm(lo(LINE)));
    a.jsr("pointer_add");
    a.bcc(next);
    a.bind(listo);
    a.inc(zp(z::OFFSET_A));
    a.jsr("expr_integer");
    a.lda(zp(z::IAC));
    a.sta(zp(z::LISTO));
    a.bind(done);
    a.jmp("prompt");
    syntax
}

/// `syntax` is LIST's Syntax error.
fn deleting(a: &mut Asm, syntax: Label) {
    // DELETE first,last: the first line from the first deleted, then each
    // after it to the last.
    a.here("statement_delete");
    a.jsr("line_argument");
    a.bcs(syntax);
    a.stx(zp(LINE_NUMBER));
    a.sty(zp(LINE_NUMBER + 1));
    a.jsr("next_a");
    a.cmp(imm(b','));
    a.bne(syntax);
    a.jsr("last_argument");
    a.bcs(syntax);
    a.jsr("find_line");
    let (delete, deleted) = (a.here_local(), a.local());
    a.ldy(imm(1));
    a.lda(zp(RECORD).ind_y());
    a.cmp(imm(END));
    a.beq(deleted);
    a.jsr("delete_record");
    // The end's number is past any LAST.
    a.ldy(imm(2));
    a.lda(zp(LAST));
    a.cmp(zp(RECORD).ind_y());
    a.dey();
    a.lda(zp(LAST + 1));
    a.sbc(zp(RECORD).ind_y());
    a.bcs(delete);
    a.bind(deleted);
    a.jsr("clear_variables");
    a.jmp("prompt");

    // A line number next, spaces passed: C clear and XY the number, the
    // pointer past it; C set if there is none, the pointer at what is.
    a.here("line_argument");
    a.jsr("next_a");
    a.cmp(imm(LINE_REFERENCE));
    let reference = a.local();
    a.beq(reference);
    a.dec(zp(z::OFFSET_A));
    a.sec();
    a.rts();
    a.bind(reference);
    a.lda(zp(z::TEXT_A));
    a.sta(zp(LINE));
    a.lda(zp(z::TEXT_A + 1));
    a.sta(zp(LINE + 1));
    a.jsr("decode_reference");
    a.lda(zp(z::OFFSET_A));
    a.clc();
    a.adc(imm(REFERENCE - 1));
    a.sta(zp(z::OFFSET_A));
    a.rts();
}

/// The step's Silly, which RENUMBER shares.
fn numbering(a: &mut Asm) -> Label {
    // AUTO: each number from the first by the step shown and its line read
    // and entered, until Escape or past 32767.
    a.here("statement_auto");
    a.jsr("numbers_argument");
    let (line, done) = (a.here_local(), a.local());
    a.lda(zp(LINE_NUMBER + 1));
    a.bmi(done);
    a.ldx(zp(LINE_NUMBER));
    a.tay();
    a.jsr("print_line_number");
    a.lda(imm(b' '));
    a.ldx(imm(lo("auto_block")));
    a.ldy(imm(hi("auto_block")));
    a.jsr("read_line");
    a.lda(imm(lo(AUTO_TEXT)));
    a.ldx(imm(hi(AUTO_TEXT)));
    a.jsr("crunch_statement");
    a.txa();
    a.clc();
    a.adc(imm(HEADER));
    a.sta(zp(LENGTH));
    a.jsr("store_line");
    a.lda(zp(STEP));
    a.ldx(imm(lo(LINE_NUMBER)));
    a.jsr("pointer_add");
    a.bcc(line);
    a.bind(done);
    a.jmp("prompt");

    a.here("auto_block");
    a.word(AUTO_TEXT);
    a.byte(LINE_LIMIT - HEADER);
    a.byte(b' ');
    a.byte(0xFF);

    // LINE_NUMBER the line number next, else 10; then the step.
    a.here("numbers_argument");
    a.jsr("line_argument");
    let given = a.local();
    a.bcc(given);
    a.ldx(imm(DEFAULT_STEP));
    a.ldy(imm(0));
    a.bind(given);
    a.stx(zp(LINE_NUMBER));
    a.sty(zp(LINE_NUMBER + 1));
    a.fall_into("step_argument");

    // A comma and the step after it, else the default; Silly unless from 1
    // to 255.
    a.here("step_argument");
    a.lda(imm(DEFAULT_STEP));
    a.sta(zp(STEP));
    a.jsr("next_a");
    a.cmp(imm(b','));
    let given = a.local();
    a.beq(given);
    a.dec(zp(z::OFFSET_A));
    a.rts();
    a.bind(given);
    a.jsr("line_argument");
    a.bcs(RTS);
    a.tya();
    let silly = a.local();
    a.bne(silly);
    a.txa();
    a.beq(silly);
    a.stx(zp(STEP));
    a.rts();
    a.bind(silly);
    a.jmp("err_silly");
    silly
}

/// `silly` is `step_argument`'s Silly.
fn renumbering(a: &mut Asm, silly: Label) {
    // A reference made its target's new number, or Failed at the new
    // number of its line where it has none; the scan then past it.
    let (reference, step_on) = (a.local(), a.local());
    a.bind(reference);
    a.iny();
    a.sty(zp(WRITE));
    a.lda(zp(RECORD));
    a.sta(zp(LINE));
    a.sta(zp(FROM));
    a.lda(zp(RECORD + 1));
    a.sta(zp(LINE + 1));
    a.sta(zp(FROM + 1));
    a.jsr("decode_reference");
    a.stx(zp(TARGET));
    a.sty(zp(TARGET + 1));
    a.jsr("find_target");
    let failed = a.local();
    a.bcs(failed);
    a.stx(zp(NUMBER));
    a.sty(zp(NUMBER + 1));
    // Y is left at the reference's last byte.
    a.jsr("encode_number");
    a.jmp(step_on);
    a.bind(failed);
    a.ldx(imm(FAILED_AT));
    a.jsr("print_message");
    a.ldx(zp(GIVEN));
    a.ldy(zp(GIVEN + 1));
    a.jsr("print_unsigned");
    a.jsr("print_newline");
    a.lda(zp(WRITE));
    a.clc();
    a.adc(imm(REFERENCE - 2));
    a.tay();
    a.bne(step_on);

    // RENUMBER first,step: Silly unless every new number is at most 32767;
    // each reference made its target's new number; then the lines
    // renumbered.
    a.here("statement_renumber");
    a.jsr("numbers_argument");
    let (fit, fits) = (a.local(), a.local());
    a.jsr("first_record");
    a.beq(fits);
    a.bind(fit);
    a.lda(zp(GIVEN + 1));
    a.bmi(silly);
    a.jsr("next_record");
    a.bne(fit);
    a.bind(fits);
    let (record, scan, line_done, numbers) = (a.local(), a.local(), a.local(), a.local());
    a.jsr("first_record");
    a.beq(numbers);
    a.bind(record);
    a.ldy(imm(HEADER));
    a.bind(scan);
    a.lda(zp(RECORD).ind_y());
    a.cmp(imm(CR));
    a.beq(line_done);
    a.cmp(imm(LINE_REFERENCE));
    a.beq(reference);
    a.bind(step_on);
    a.iny();
    a.bne(scan);
    a.bind(line_done);
    a.jsr("next_record");
    a.bne(record);
    a.bind(numbers);
    let (number, renumbered) = (a.local(), a.local());
    a.jsr("first_record");
    a.beq(renumbered);
    a.bind(number);
    a.lda(zp(GIVEN + 1));
    a.sta(zp(RECORD).ind_y());
    a.iny();
    a.lda(zp(GIVEN));
    a.sta(zp(RECORD).ind_y());
    a.jsr("next_record");
    a.bne(number);
    a.bind(renumbered);
    a.jmp("prompt");
}

fn records(a: &mut Asm) {
    // RECORD on to the next record and GIVEN to its number; then as
    // first_record.
    a.here("next_record");
    a.ldy(imm(3));
    a.lda(zp(RECORD).ind_y());
    a.ldx(imm(lo(RECORD)));
    a.jsr("pointer_add");
    a.lda(zp(STEP));
    a.ldx(imm(lo(GIVEN)));
    a.jsr("pointer_add");
    let ended = a.local();
    a.bcc(ended);
    // RECORD at the first record and GIVEN the first number; Z set and Y 1
    // at the program's end.
    a.here("first_record");
    a.ldx(imm(lo(RECORD)));
    a.jsr("page_pointer");
    a.lda(zp(LINE_NUMBER));
    a.sta(zp(GIVEN));
    a.lda(zp(LINE_NUMBER + 1));
    a.sta(zp(GIVEN + 1));
    a.bind(ended);
    a.ldy(imm(1));
    a.lda(zp(RECORD).ind_y());
    a.cmp(imm(END));
    a.rts();

    // C clear and XY the new number of line TARGET; C set if no line has
    // that number. SOURCE walks the records, WALKED_NUMBER their numbers.
    a.here("find_target");
    a.ldx(imm(lo(SOURCE)));
    a.jsr("page_pointer");
    a.lda(zp(LINE_NUMBER));
    a.sta(zp(WALKED_NUMBER));
    a.lda(zp(LINE_NUMBER + 1));
    a.sta(zp(WALKED_NUMBER + 1));
    let (search, missing, found) = (a.here_local(), a.local(), a.local());
    a.ldy(imm(1));
    a.lda(zp(SOURCE).ind_y());
    a.cmp(imm(END));
    a.beq(missing);
    a.iny();
    a.lda(zp(SOURCE).ind_y());
    a.cmp(zp(TARGET));
    a.dey();
    a.lda(zp(SOURCE).ind_y());
    a.sbc(zp(TARGET + 1));
    let below = a.local();
    a.bcc(below);
    // At or past TARGET: equal or missing.
    a.ora(imm(0));
    a.bne(missing);
    a.iny();
    a.lda(zp(SOURCE).ind_y());
    a.cmp(zp(TARGET));
    a.beq(found);
    a.bind(missing);
    a.sec();
    a.rts();
    a.bind(found);
    a.ldx(zp(WALKED_NUMBER));
    a.ldy(zp(WALKED_NUMBER + 1));
    a.clc();
    a.rts();
    a.bind(below);
    a.ldy(imm(3));
    a.lda(zp(SOURCE).ind_y());
    a.ldx(imm(lo(SOURCE)));
    a.jsr("pointer_add");
    a.lda(zp(STEP));
    a.ldx(imm(lo(WALKED_NUMBER)));
    a.jsr("pointer_add");
    a.bcc(search);
}

fn files(a: &mut Asm) {
    // SAVE: from PAGE to TOP, loaded at PAGE and run at the entry that
    // keeps the program.
    a.here("statement_save");
    a.jsr("file_block");
    a.lda(zp(z::PAGE));
    a.sta(zp(FILE_BLOCK + 11));
    a.lda(imm(lo(SAVED_EXEC)));
    a.sta(zp(FILE_BLOCK + 6));
    a.lda(imm(hi(SAVED_EXEC)));
    a.sta(zp(FILE_BLOCK + 7));
    a.lda(zp(z::TOP));
    a.sta(zp(FILE_BLOCK + 14));
    a.lda(zp(z::TOP + 1));
    a.sta(zp(FILE_BLOCK + 15));
    a.lda(imm(OSFILE_SAVE));
    a.sta(zp(FILE_BLOCK + 10));
    a.jsr("osfile");
    a.jmp("statement_done");

    // LOAD: the file at PAGE, then the prompt. CHAIN: then RUN.
    a.here("statement_load");
    a.jsr("load_file");
    a.jmp("prompt_fresh");
    a.here("statement_chain");
    a.jsr("load_file");
    a.jsr("fresh_program");
    a.jmp("statement_run");

    // The name's string ended with a CR, and the block naming it, every
    // address in the I/O processor, the load address PAGE and, for a load,
    // the address the block gives.
    a.here("file_block");
    a.jsr("expr_string");
    a.dec(zp(z::OFFSET_A));
    a.ldy(zp(z::STRING_LENGTH));
    a.lda(imm(CR));
    a.sta(abs(page::STRING).y());
    a.ldx(imm(FILE_BLOCK_LENGTH - 1));
    a.lda(imm(0xFF));
    let fill = a.here_local();
    a.sta(zp(FILE_BLOCK).x());
    a.dex();
    a.bpl(fill);
    a.lda(imm(0));
    a.sta(zp(FILE_BLOCK));
    a.sta(zp(FILE_BLOCK + 2));
    a.sta(zp(FILE_BLOCK + 6));
    a.lda(imm(hi(page::STRING)));
    a.sta(zp(FILE_BLOCK + 1));
    a.lda(zp(z::PAGE));
    a.sta(zp(FILE_BLOCK + 3));
    a.rts();

    a.here("load_file");
    a.jsr("file_block");
    a.lda(imm(OSFILE_LOAD));
    a.here("osfile");
    a.ldx(imm(lo(FILE_BLOCK)));
    a.ldy(imm(hi(FILE_BLOCK)));
    a.jmp(abs(entry::OSFILE));
}
