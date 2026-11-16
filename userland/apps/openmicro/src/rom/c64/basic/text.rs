//! The program's text: the cold and warm starts, the main loop that reads
//! a line, tokenises it and stores or runs it, the line search, the links
//! between lines, NEW, CLR and LIST.

use super::errors::number;
use super::tokens::token;
use super::ws::{page, vector, zp};
use super::{chrget, fixed};
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Acc, Asm, AsmError, Label, RTS};
use crate::rom::c64::kernal::entry as kernal;
use crate::rom::c64::kernal::files::status;
use crate::rom::c64::kernal::ws::zp as kzp;

/// Where the stack starts after an error or a CLR.
pub const STACK_BASE: u8 = 0xFA;
/// The seed RND starts from.
const RND_SEED: [u8; 5] = [0x80, 0x4F, 0xC7, 0x52, 0x58];
/// The `JMP` opcode, for USR's vector and JMPER.
pub const JMP: u8 = crate::cpu::isa::JMP_ABSOLUTE;
/// A line's link and number before its text.
const LINE_HEADER: u8 = 4;
/// Past this line number's high byte, another digit runs past 63999.
const LINE_DIGIT_LIMIT: u8 = 0x19;
/// `find_line`'s result: the line found.
pub const LINE_FOUND: u16 = 0x5F;
/// A block move's length, and the room an inserted line takes.
const MOVE_LENGTH: u16 = zp::FRESPC;
const MOVE_SIZE: u16 = zp::CHARAC;
/// The banner and the free bytes' message.
const BANNER: &str = "\r    **** OPENMICRO 64 BASIC V2 ****\r\r 64K RAM SYSTEM  ";
const BYTES_FREE: &str = " BASIC BYTES FREE\r";

/// The flowing code; the routines at fixed addresses are pinned by the
/// module above through [`cold_start`], [`warm_start`], [`main_loop`],
/// [`crunch`] and [`qplop`].
pub fn emit(a: &mut Asm) -> Result<(), AsmError> {
    defaults(a)?;
    a.whole(main_entries);
    a.whole(read_line);
    a.whole(line_number);
    a.whole(find_line);
    a.whole(store_line);
    a.whole(link_program);
    a.whole(|a| {
        clear(a);
        moving(a);
    });
    a.whole(list);
    a.later(texts);
    Ok(())
}

fn texts(a: &mut Asm) {
    a.here("banner_text");
    a.bytes(BANNER.as_bytes());
    a.byte(0);
    a.here("bytes_free_text");
    a.bytes(BYTES_FREE.as_bytes());
    a.byte(0);
    // Page 3 from IERROR: the vectors, SYS's registers, USR's jump.
    a.here("page_three_defaults");
    for address in [
        fixed::ERROR,
        fixed::MAIN,
        fixed::CRUNCH,
        fixed::QPLOP,
        fixed::GONE,
        fixed::EVAL,
    ] {
        a.word(address);
    }
    a.bytes(&[0; SYS_REGISTERS]);
    a.byte(JMP);
    a.word(fixed::ILLEGAL_QUANTITY);
}

/// SYS's registers, from SAREG to SPREG.
const SYS_REGISTERS: usize = 4;
const _: () = assert!(
    vector::IERROR as usize + vector::BYTES as usize + SYS_REGISTERS == page::USRPOK as usize
);

/// The cold start: BASIC's vectors and zero page, the memory BASIC has
/// from the KERNAL, NEW, the banner, READY. The warm start: output to the
/// screen, then READY.
fn defaults(a: &mut Asm) -> Result<(), AsmError> {
    let code = chrget::code()?;
    // RNDX follows CHRGET in zero page.
    a.later(move |a| {
        a.here("zero_page_defaults");
        a.bytes(&code);
        a.bytes(&RND_SEED);
    });
    Ok(())
}

/// The zero page BASIC's cold start copies: CHRGET's routine, then RND's
/// seed after it.
const ZERO_PAGE_DEFAULTS: usize = (zp::RNDX - zp::CHRGET) as usize + RND_SEED.len();

pub(super) fn cold_start(a: &mut Asm) {
    a.here("basic_cold");
    let vectors = a.local();
    let copy = a.local();
    a.ldx(imm(vector::BYTES
        + crate::rom::asm::byte(SYS_REGISTERS)
        + 2));
    a.bind(vectors);
    a.lda(abs("page_three_defaults").x());
    a.sta(abs(vector::IERROR).x());
    a.dex();
    a.bpl(vectors);
    a.ldx(imm(crate::rom::asm::byte(ZERO_PAGE_DEFAULTS - 1)));
    a.bind(copy);
    a.lda(abs("zero_page_defaults").x());
    a.sta(z(zp::CHRGET).x());
    a.dex();
    a.bpl(copy);
    for (pointer, target) in [
        (zp::ADRAY1, fixed::FAC_TO_AY),
        (zp::ADRAY2, fixed::AY_TO_FAC),
    ] {
        a.lda(imm(lo(target)));
        a.sta(z(pointer));
        a.lda(imm(hi(target)));
        a.sta(z(pointer + 1));
    }
    a.lda(imm(JMP));
    a.sta(z(zp::JMPER));
    a.sec();
    a.jsr(kernal::MEMBOT);
    a.stx(z(zp::TXTTAB));
    a.sty(z(zp::TXTTAB + 1));
    a.sec();
    a.jsr(kernal::MEMTOP);
    a.stx(z(zp::MEMSIZ));
    a.sty(z(zp::MEMSIZ + 1));
    a.ldy(imm(0));
    a.tya();
    a.sta(z(zp::TXTTAB).ind_y());
    a.inc(z(zp::TXTTAB));
    let no_carry = a.local();
    a.bne(no_carry);
    a.inc(z(zp::TXTTAB + 1));
    a.bind(no_carry);
    a.jsr("new_program");
    a.lda(imm(lo("banner_text")));
    a.ldy(imm(hi("banner_text")));
    a.jsr("print_string");
    a.lda(z(zp::MEMSIZ));
    a.sec();
    a.sbc(z(zp::TXTTAB));
    a.tax();
    a.lda(z(zp::MEMSIZ + 1));
    a.sbc(z(zp::TXTTAB + 1));
    a.jsr("print_unsigned");
    a.lda(imm(lo("bytes_free_text")));
    a.ldy(imm(hi("bytes_free_text")));
    a.jsr("print_string");
    a.jmp("ready");
}

pub(super) fn warm_start(a: &mut Asm) {
    a.here("basic_warm");
    a.jsr("restore_output");
    a.jsr("reset_stacks");
    a.cli();
    a.ldx(imm(number::READY));
    a.jmp(abs(vector::IERROR).ind());
}

/// `crunch_line`: the tokeniser through ICRNCH, TXTPTR then at the
/// buffer's start.
fn main_entries(a: &mut Asm) {
    a.here("crunch_line");
    a.jmp(abs(vector::ICRNCH).ind());
}

/// `main`: the main loop through IMAIN, whose default `main_loop` follows
/// it: a line read; with a number, stored; without, tokenised and run as a
/// direct command.
pub(super) fn main_loop(a: &mut Asm) {
    a.here("main");
    a.jmp(abs(vector::IMAIN).ind());
    a.here("main_loop");
    let numbered = a.local();
    a.jsr("read_line");
    a.stx(z(zp::TXTPTR));
    a.sty(z(zp::TXTPTR + 1));
    a.jsr(zp::CHRGET);
    a.tax();
    a.beq("main");
    a.ldx(imm(super::errors::DIRECT_MODE));
    a.stx(z(zp::CURLIN + 1));
    a.bcc(numbered);
    a.jsr("crunch_line");
    a.jmp("run_direct");
    a.bind(numbered);
    a.jsr("parse_line_number");
    a.jsr("crunch_line");
    a.jmp("store_line");
}

/// `read_line_quietly`: a line read through CHRIN into the buffer, ended
/// by a zero; X (low) and Y point before it. Y counts, as CHRIN may change
/// X. From a file, a character read as the device timed out, which it never
/// sent, ends the line unkept. Past the buffer's room is STRING TOO LONG.
/// `read_line`: the same, then a new line printed unless a file has the
/// output.
fn read_line(a: &mut Asm) {
    a.here("read_line");
    a.jsr("read_line_quietly");
    a.lda(z(zp::CHANNL));
    a.bne("before_buffer");
    a.jsr("print_newline");
    a.here("before_buffer");
    a.ldx(imm(lo(page::BUF - 1)));
    a.ldy(imm(hi(page::BUF - 1)));
    a.rts();

    a.here("read_line_quietly");
    let (next, kept, end) = (a.local(), a.local(), a.local());
    a.ldy(imm(0));
    a.bind(next);
    a.jsr(kernal::CHRIN);
    a.cmp(imm(b'\r'));
    a.beq(end);
    a.sta(abs(page::BUF).y());
    a.ldx(z(zp::CHANNL));
    a.beq(kept);
    a.lda(z(kzp::STATUS));
    a.and(imm(status::READ_TIMEOUT));
    a.bne(end);
    a.bind(kept);
    a.iny();
    a.cpy(imm(page::BUF_BYTES));
    a.bcc(next);
    a.jmp("string_too_long_error");
    a.bind(end);
    a.lda(imm(0));
    a.sta(abs(page::BUF).y());
    a.beq("before_buffer");
}

/// GARBFL while tokenising: within quotes, and within DATA.
const QUOTED: u8 = 0x80;
const IN_DATA: u8 = 0x40;

/// `crunch`, ICRNCH's default: the text from TXTPTR tokenised into the
/// buffer's start — each keyword, or an abbreviation of it with its last
/// letter shifted, the first in token order to match; `?` is PRINT;
/// nothing in quotes, in DATA up to a colon outside quotes, or after
/// REM — then three zeros; TXTPTR before the buffer.
pub(super) fn crunch(a: &mut Asm) {
    a.here("crunch");
    let next = a.local();
    let not_quote = a.local();
    let not_print = a.local();
    let copy = a.local();
    let raw = a.local();
    let end = a.local();
    let search = a.local();
    a.lda(z(zp::TXTPTR));
    a.sec();
    a.sbc(imm(lo(page::BUF)));
    a.tax();
    a.ldy(imm(0));
    a.sty(z(zp::GARBFL));
    a.bind(next);
    a.lda(abs(page::BUF).x());
    a.beq(end);
    a.cmp(imm(b'"'));
    a.bne(not_quote);
    a.lda(z(zp::GARBFL));
    a.eor(imm(QUOTED));
    a.sta(z(zp::GARBFL));
    a.lda(imm(b'"'));
    a.bne(copy);
    a.bind(not_quote);
    a.bit(z(zp::GARBFL));
    a.bmi(copy);
    a.bvs(raw);
    a.cmp(imm(b'?'));
    a.bne(not_print);
    a.lda(imm(token::PRINT));
    a.bne(copy);
    a.bind(not_print);
    a.cmp(imm(b' '));
    a.beq(copy);
    a.cmp(imm(b'0'));
    a.bcc(search);
    a.cmp(imm(b'<'));
    a.bcc(copy);
    a.cmp(imm(0x80));
    a.bcc(search);
    a.bind(copy);
    a.sta(abs(page::BUF).y());
    a.iny();
    a.inx();
    a.bne(next);
    // In DATA outside quotes, a colon ends it.
    a.bind(raw);
    a.cmp(imm(b':'));
    a.bne(copy);
    a.lda(imm(0));
    a.sta(z(zp::GARBFL));
    a.lda(imm(b':'));
    a.bne(copy);
    a.bind(end);
    a.sta(abs(page::BUF).y());
    a.sta(abs(page::BUF + 1).y());
    a.sta(abs(page::BUF + 2).y());
    a.ldx(imm(lo(page::BUF - 1)));
    a.stx(z(zp::TXTPTR));
    a.ldx(imm(hi(page::BUF - 1)));
    a.stx(z(zp::TXTPTR + 1));
    a.rts();
    keyword_search(a, search, copy, end);
}

/// `crunch` run on at `search`: each keyword compared with the text from
/// COUNT, CHARAC its token; one found is copied as its token at `copy`, and
/// after REM the rest of the line as it is, to `end`.
fn keyword_search(a: &mut Asm, search: Label, copy: Label, end: Label) {
    let (keyword, compare, same, skip, hit, not_data, remark) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.bind(search);
    a.stx(z(zp::COUNT));
    a.sty(z(zp::ENDCHR));
    a.ldy(imm(0));
    a.lda(imm(token::END));
    a.sta(z(zp::CHARAC));
    a.bind(keyword);
    a.ldx(z(zp::COUNT));
    a.bind(compare);
    a.lda(abs(page::BUF).x());
    a.sec();
    a.sbc(abs("keywords").y());
    a.beq(same);
    a.cmp(imm(0x80));
    a.beq(hit);
    a.bind(skip);
    a.lda(abs("keywords").y());
    a.iny();
    a.asl(Acc);
    a.bcc(skip);
    a.inc(z(zp::CHARAC));
    a.lda(abs("keywords").y());
    a.bne(keyword);
    a.ldx(z(zp::COUNT));
    a.ldy(z(zp::ENDCHR));
    a.lda(abs(page::BUF).x());
    a.bne(copy);
    a.bind(same);
    a.lda(abs("keywords").y());
    a.bmi(hit);
    a.inx();
    a.iny();
    a.bne(compare);
    a.bind(hit);
    a.lda(z(zp::CHARAC));
    a.cmp(imm(token::DATA));
    a.bne(not_data);
    a.ldy(imm(IN_DATA));
    a.sty(z(zp::GARBFL));
    a.bind(not_data);
    a.ldy(z(zp::ENDCHR));
    a.cmp(imm(token::REM));
    a.bne(copy);
    // After REM, the rest as it is.
    a.bind(remark);
    a.sta(abs(page::BUF).y());
    a.inx();
    a.iny();
    a.lda(abs(page::BUF).x());
    a.bne(remark);
    a.beq(end);
}

/// `parse_line_number`: LINNUM the line number at TXTPTR, from the character
/// CHRGOT leaves in A with C clear for a digit; past 63999 is SYNTAX.
fn line_number(a: &mut Asm) {
    a.here("parse_line_number");
    let digit = a.local();
    let in_range = a.local();
    let added = a.local();
    a.ldx(imm(0));
    a.stx(z(zp::LINNUM));
    a.stx(z(zp::LINNUM + 1));
    a.bind(digit);
    a.bcs(RTS);
    a.sbc(imm(b'0' - 1));
    a.pha();
    a.lda(z(zp::LINNUM + 1));
    a.cmp(imm(LINE_DIGIT_LIMIT));
    a.bcc(in_range);
    a.jmp("syntax_error");
    a.bind(in_range);
    a.lda(z(zp::LINNUM));
    a.ldy(z(zp::LINNUM + 1));
    for _ in 0..2 {
        a.asl(z(zp::LINNUM));
        a.rol(z(zp::LINNUM + 1));
    }
    // Below 6400 no step of the times ten carries out of 16 bits.
    a.adc(z(zp::LINNUM));
    a.sta(z(zp::LINNUM));
    a.tya();
    a.adc(z(zp::LINNUM + 1));
    a.sta(z(zp::LINNUM + 1));
    a.asl(z(zp::LINNUM));
    a.rol(z(zp::LINNUM + 1));
    a.pla();
    a.adc(z(zp::LINNUM));
    a.sta(z(zp::LINNUM));
    a.bcc(added);
    a.inc(z(zp::LINNUM + 1));
    a.bind(added);
    a.jsr(zp::CHRGET);
    a.jmp(digit);
}

/// `find_line`: [`LINE_FOUND`] the first line from the program's start
/// numbered LINNUM or more, C set if it is LINNUM; `find_line_from` starts at
/// the line at A (low) and X.
fn find_line(a: &mut Asm) {
    a.here("find_line");
    a.lda(z(zp::TXTTAB));
    a.ldx(z(zp::TXTTAB + 1));
    a.here("find_line_from");
    let next = a.local();
    let greater = a.local();
    a.sta(z(LINE_FOUND));
    a.stx(z(LINE_FOUND + 1));
    a.ldy(imm(1));
    a.lda(z(LINE_FOUND).ind_y());
    a.beq(greater);
    a.ldy(imm(3));
    a.lda(z(LINE_FOUND).ind_y());
    a.cmp(z(zp::LINNUM + 1));
    a.bcc(next);
    a.bne(greater);
    a.dey();
    a.lda(z(LINE_FOUND).ind_y());
    a.cmp(z(zp::LINNUM));
    a.bcc(next);
    a.beq(RTS);
    a.bind(greater);
    a.clc();
    a.rts();
    // C is clear from either comparison.
    a.bind(next);
    a.ldy(imm(1));
    a.lda(z(LINE_FOUND).ind_y());
    a.tax();
    a.dey();
    a.lda(z(LINE_FOUND).ind_y());
    a.bcc("find_line_from");
}

/// `store_line`: the line numbered LINNUM replaced by the tokenised
/// buffer, or deleted by an empty one; the lines relinked, the variables
/// cleared, nothing left for CONT, then the main loop. The variables go,
/// so the program may grow to MEMSIZ; past it is OUT OF MEMORY, the old
/// line gone.
fn store_line(a: &mut Asm) {
    a.here("store_line");
    let absent = a.local();
    let relink = a.local();
    let measure = a.local();
    let room = a.local();
    let copy = a.local();
    let no_room = a.local();
    a.jsr("find_line");
    a.bcc(absent);
    // Deleted: the lines after it moved down over it.
    a.ldy(imm(0));
    a.lda(z(LINE_FOUND).ind_y());
    a.sta(z(zp::INDEX));
    a.iny();
    a.lda(z(LINE_FOUND).ind_y());
    a.sta(z(zp::INDEX + 1));
    a.lda(z(LINE_FOUND));
    a.sta(z(zp::INDEX2));
    a.lda(z(LINE_FOUND + 1));
    a.sta(z(zp::INDEX2 + 1));
    a.lda(z(zp::VARTAB));
    a.sec();
    a.sbc(z(zp::INDEX));
    a.sta(z(MOVE_LENGTH));
    a.lda(z(zp::VARTAB + 1));
    a.sbc(z(zp::INDEX + 1));
    a.sta(z(MOVE_LENGTH + 1));
    a.lda(z(LINE_FOUND));
    a.clc();
    a.adc(z(MOVE_LENGTH));
    a.sta(z(zp::VARTAB));
    a.lda(z(LINE_FOUND + 1));
    a.adc(z(MOVE_LENGTH + 1));
    a.sta(z(zp::VARTAB + 1));
    a.jsr("move_forward");
    a.bind(absent);
    a.lda(abs(page::BUF));
    a.beq(relink);
    // Inserted: its size, room made at it, then its header and text.
    a.ldy(imm(0));
    a.bind(measure);
    a.iny();
    a.lda(abs(page::BUF - 1).y());
    a.bne(measure);
    a.tya();
    a.clc();
    a.adc(imm(LINE_HEADER));
    a.pha();
    a.clc();
    a.adc(z(zp::VARTAB));
    a.tax();
    a.lda(z(zp::VARTAB + 1));
    a.adc(imm(0));
    a.cmp(z(zp::MEMSIZ + 1));
    a.bcc(room);
    a.bne(no_room);
    a.cpx(z(zp::MEMSIZ));
    a.bcs(no_room);
    a.bind(room);
    a.lda(z(LINE_FOUND));
    a.sta(z(zp::INDEX));
    a.lda(z(LINE_FOUND + 1));
    a.sta(z(zp::INDEX + 1));
    a.pla();
    a.ldx(imm(lo(zp::VARTAB)));
    a.jsr("open_gap");
    // Any link but zero, which ends the program, until the relinking.
    a.ldy(imm(1));
    a.tya();
    a.sta(z(LINE_FOUND).ind_y());
    a.iny();
    a.lda(z(zp::LINNUM));
    a.sta(z(LINE_FOUND).ind_y());
    a.iny();
    a.lda(z(zp::LINNUM + 1));
    a.sta(z(LINE_FOUND).ind_y());
    a.ldx(imm(0xFF));
    a.bind(copy);
    a.inx();
    a.iny();
    a.lda(abs(page::BUF).x());
    a.sta(z(LINE_FOUND).ind_y());
    a.bne(copy);
    a.bind(relink);
    a.jsr("link_program");
    a.jsr("clear_all");
    a.jmp("main");
    a.bind(no_room);
    a.jsr("link_program");
    a.jsr("clear_all");
    a.jmp("out_of_memory_error");
}

/// `link_program`: each line's link set to the line after it, a line
/// whose link's high byte is zero being the end.
fn link_program(a: &mut Asm) {
    a.here("link_program");
    let line = a.local();
    let scan = a.local();
    a.lda(z(zp::TXTTAB));
    a.sta(z(zp::INDEX));
    a.lda(z(zp::TXTTAB + 1));
    a.sta(z(zp::INDEX + 1));
    a.bind(line);
    a.ldy(imm(1));
    a.lda(z(zp::INDEX).ind_y());
    a.beq(RTS);
    a.ldy(imm(LINE_HEADER));
    a.bind(scan);
    a.iny();
    a.lda(z(zp::INDEX).ind_y());
    a.bne(scan);
    a.tya();
    a.sec();
    a.adc(z(zp::INDEX));
    a.tax();
    a.ldy(imm(0));
    a.sta(z(zp::INDEX).ind_y());
    a.lda(z(zp::INDEX + 1));
    a.adc(imm(0));
    a.iny();
    a.sta(z(zp::INDEX).ind_y());
    a.stx(z(zp::INDEX));
    a.sta(z(zp::INDEX + 1));
    // Lines lie above page zero.
    a.bne(line);
}

/// `new_program`: an empty program, then `clear_all`. `clear_all`: CLR —
/// the files closed, then `clear_open`. `clear_open`: no variables, arrays
/// or strings, READ at the first DATA, nothing for CONT, then
/// `reset_stacks`. `reset_stacks`: the stack emptied but for the caller's
/// return, no temporary strings. `restore_data` and `text_start`: DATPTR
/// and TXTPTR before the program's first line.
fn clear(a: &mut Asm) {
    a.here("new_program");
    a.lda(imm(0));
    a.tay();
    a.sta(z(zp::TXTTAB).ind_y());
    a.iny();
    a.sta(z(zp::TXTTAB).ind_y());
    a.tya();
    a.sec();
    a.adc(z(zp::TXTTAB));
    a.sta(z(zp::VARTAB));
    a.lda(z(zp::TXTTAB + 1));
    a.adc(imm(0));
    a.sta(z(zp::VARTAB + 1));
    a.fall_into("clear_all");
    a.here("clear_all");
    a.jsr(kernal::CLALL);
    a.here("clear_open");
    a.lda(z(zp::MEMSIZ));
    a.sta(z(zp::FRETOP));
    a.lda(z(zp::MEMSIZ + 1));
    a.sta(z(zp::FRETOP + 1));
    for byte in [0, 1] {
        a.lda(z(zp::VARTAB + byte));
        a.sta(z(zp::ARYTAB + byte));
        a.sta(z(zp::STREND + byte));
    }
    a.jsr("restore_data");
    a.lda(imm(0));
    a.sta(z(zp::OLDTXT + 1));
    a.fall_into("reset_stacks");

    a.here("reset_stacks");
    a.pla();
    a.tay();
    a.pla();
    a.ldx(imm(STACK_BASE));
    a.txs();
    a.pha();
    a.tya();
    a.pha();
    a.lda(imm(zp::TEMPST.to_le_bytes()[0]));
    a.sta(z(zp::TEMPPT));
    a.lda(imm(0));
    a.sta(z(zp::SUBFLG));
    a.rts();

    a.here("restore_data");
    a.ldx(imm(lo(zp::DATPTR)));
    a.skip_over(|a| {
        a.here("text_start");
        a.ldx(imm(zp::TXTPTR.to_le_bytes()[0]));
    });
    a.skip_over(|a| {
        a.here("cont_start");
        a.ldx(imm(zp::OLDTXT.to_le_bytes()[0]));
    });
    a.lda(z(zp::TXTTAB));
    a.sec();
    a.sbc(imm(1));
    a.sta(z(0).x());
    a.lda(z(zp::TXTTAB + 1));
    a.sbc(imm(0));
    a.sta(z(1).x());
    a.rts();
}

/// `move_forward`: a block moved down, `MOVE_LENGTH` bytes from INDEX to
/// INDEX2, forward. `open_gap`: the bytes from INDEX to the pointer in zero
/// page X moved up A bytes, the pointer with them; the length's part page
/// goes first, from INDEX and INDEX2 moved up its whole pages, then each
/// whole page down.
fn moving(a: &mut Asm) {
    a.here("move_forward");
    let page_loop = a.local();
    let partial = a.local();
    let byte_loop = a.local();
    a.ldy(imm(0));
    a.ldx(z(MOVE_LENGTH + 1));
    a.beq(partial);
    a.bind(page_loop);
    a.lda(z(zp::INDEX).ind_y());
    a.sta(z(zp::INDEX2).ind_y());
    a.iny();
    a.bne(page_loop);
    a.inc(z(zp::INDEX + 1));
    a.inc(z(zp::INDEX2 + 1));
    a.dex();
    a.bne(page_loop);
    a.bind(partial);
    a.ldx(z(MOVE_LENGTH));
    a.beq(RTS);
    a.bind(byte_loop);
    a.lda(z(zp::INDEX).ind_y());
    a.sta(z(zp::INDEX2).ind_y());
    a.iny();
    a.dex();
    a.bne(byte_loop);
    a.rts();

    a.here("open_gap");
    let next = a.local();
    let copy = a.local();
    a.sta(z(MOVE_SIZE));
    a.lda(z(0).x());
    a.sec();
    a.sbc(z(zp::INDEX));
    a.tay();
    a.lda(z(1).x());
    a.sbc(z(zp::INDEX + 1));
    a.pha();
    a.clc();
    a.adc(z(zp::INDEX + 1));
    a.sta(z(zp::INDEX + 1));
    a.lda(z(zp::INDEX));
    a.adc(z(MOVE_SIZE));
    a.sta(z(zp::INDEX2));
    a.lda(z(zp::INDEX + 1));
    a.adc(imm(0));
    a.sta(z(zp::INDEX2 + 1));
    a.lda(z(MOVE_SIZE));
    a.jsr("add_to_pointer");
    a.pla();
    a.tax();
    a.inx();
    a.tya();
    a.bne(copy);
    a.bind(next);
    a.dex();
    a.beq(RTS);
    a.dec(z(zp::INDEX + 1));
    a.dec(z(zp::INDEX2 + 1));
    a.bind(copy);
    a.dey();
    a.lda(z(zp::INDEX).ind_y());
    a.sta(z(zp::INDEX2).ind_y());
    a.tya();
    a.bne(copy);
    a.beq(next);
}

/// LIST's line and its offset into the line's text; LINNUM holds the
/// last number.
const LIST_LINE: u16 = zp::FORPNT;
const LIST_OFFSET: u16 = zp::OPMASK;

/// LIST: the lines from a first number to a last, either left out; each
/// on a new line as its number, a space and its text, keywords spelled
/// out through IQPLOP except within quotes; the new line comes before
/// the number is checked against the last. STOP ends the listing.
fn list(a: &mut Asm) {
    a.here("st_list");
    let to_end = a.local();
    let syntax = a.local();
    let line = a.local();
    let text = a.local();
    let not_quote = a.local();
    let next = a.local();
    let done = a.local();
    // The entry's flags: Z for no range, C clear for a first number, which
    // ends the range itself unless a dash follows.
    a.php();
    a.jsr("parse_line_number");
    a.jsr("find_line");
    a.lda(z(LINE_FOUND));
    a.sta(z(LIST_LINE));
    a.lda(z(LINE_FOUND + 1));
    a.sta(z(LIST_LINE + 1));
    a.plp();
    a.beq(to_end);
    a.jsr(zp::CHRGOT);
    a.beq(line);
    a.cmp(imm(token::MINUS));
    a.bne(syntax);
    a.jsr(zp::CHRGET);
    a.bcs(to_end);
    a.jsr("parse_line_number");
    a.bcs(line);
    a.bind(to_end);
    a.ldx(imm(0xFF));
    a.stx(z(zp::LINNUM));
    a.stx(z(zp::LINNUM + 1));

    a.bind(line);
    a.ldy(imm(1));
    a.lda(z(LIST_LINE).ind_y());
    a.beq(done);
    a.jsr(kernal::STOP);
    a.beq(done);
    a.jsr("print_newline");
    a.ldy(imm(2));
    a.lda(z(zp::LINNUM));
    a.cmp(z(LIST_LINE).ind_y());
    a.iny();
    a.lda(z(zp::LINNUM + 1));
    a.sbc(z(LIST_LINE).ind_y());
    a.bcc(done);
    a.dey();
    a.lda(z(LIST_LINE).ind_y());
    a.tax();
    a.iny();
    a.lda(z(LIST_LINE).ind_y());
    a.jsr("print_unsigned");
    a.lda(imm(b' '));
    a.jsr("print_char");
    a.lda(imm(LINE_HEADER));
    a.sta(z(LIST_OFFSET));
    a.lda(imm(0));
    a.sta(z(zp::GARBFL));
    a.bind(text);
    a.ldy(z(LIST_OFFSET));
    a.lda(z(LIST_LINE).ind_y());
    a.beq(next);
    a.cmp(imm(b'"'));
    a.bne(not_quote);
    a.lda(z(zp::GARBFL));
    a.eor(imm(QUOTED));
    a.sta(z(zp::GARBFL));
    a.lda(imm(b'"'));
    a.bind(not_quote);
    a.jsr("list_character");
    a.inc(z(LIST_OFFSET));
    a.bne(text);
    a.bind(next);
    a.ldy(imm(0));
    a.lda(z(LIST_LINE).ind_y());
    a.tax();
    a.iny();
    a.lda(z(LIST_LINE).ind_y());
    a.stx(z(LIST_LINE));
    a.sta(z(LIST_LINE + 1));
    // The line listed had a link, so the next's high byte is not zero.
    a.bne(line);
    a.bind(done);
    a.jmp("ready");
    a.bind(syntax);
    a.jmp("syntax_error");

    a.here("list_character");
    a.jmp(abs(vector::IQPLOP).ind());
}

/// QPLOP's default: a token's keyword, or the character, within quotes
/// always the character.
pub(super) fn qplop(a: &mut Asm) {
    a.here("qplop");
    let raw = a.local();
    let find = a.local();
    let skip = a.local();
    let print = a.local();
    a.bit(z(zp::GARBFL));
    a.bmi(raw);
    a.cmp(imm(0x80));
    a.bcc(raw);
    a.cmp(imm(token::GO + 1));
    a.bcs(raw);
    a.here("keyword");
    a.sec();
    a.sbc(imm(0x7F));
    a.tax();
    a.ldy(imm(0));
    a.bind(find);
    a.dex();
    a.beq(print);
    a.bind(skip);
    a.lda(abs("keywords").y());
    a.iny();
    a.asl(Acc);
    a.bcc(skip);
    a.bcs(find);
    a.bind(print);
    a.lda(abs("keywords").y());
    a.pha();
    a.and(imm(0x7F));
    a.jsr("print_char");
    a.iny();
    a.pla();
    a.bpl(print);
    a.rts();
    a.bind(raw);
    a.jmp("print_char");
}
