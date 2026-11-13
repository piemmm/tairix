//! The utilities, which work on any filing system through the OS's file
//! calls: *TYPE, *LIST with its four-digit line numbers, *DUMP eight bytes
//! a line in hex and as characters, *BUILD from typed lines, and *DISC,
//! which selects the DFS.
//!
//! As DFS 1.20's do, *TYPE and *LIST end with a new line, and *DUMP shows
//! a row past the end as `**` when a file fills its last row exactly.

use super::far;
use super::ws::{page, private, zp as z};
use crate::rom::asm::{abs, imm, zp, Asm, RTS};
use crate::rom::os::{entry, page_d, zp as os, ESCAPE_FLAG};

const CR: u8 = 0x0D;
/// OSFIND's modes for reading and writing.
const OPEN_IN: u8 = 0x40;
const OPEN_OUT: u8 = 0x80;
/// *DUMP's bytes to a row, and what a byte past the end shows as.
const ROW: u8 = 8;
const PAST: u8 = b'*';
/// The printable characters *DUMP shows as themselves.
const FIRST_SHOWN: u8 = b' ';
const LAST_SHOWN: u8 = b'~';
/// *BUILD's line: the longest, and the characters it takes.
const LINE_MAX: u8 = 0xFF;
const LINE_LOW: u8 = b' ';
const LINE_HIGH: u8 = 0xFF;

/// The handle, BCD line number and a flag the utilities keep while they
/// run, in the utility zero page.
mod u {
    use super::z;
    pub const HANDLE: u16 = z::UTILITY;
    pub const LINE: u16 = z::UTILITY + 1;
    pub const AT_START: u16 = z::UTILITY + 3;
    pub const OFFSET: u16 = z::UTILITY + 4;
    pub const COUNT: u16 = z::UTILITY + 6;
    pub const ENDED: u16 = z::UTILITY + 7;
}

pub(super) fn emit(a: &mut Asm) {
    opening(a);
    type_and_list(a);
    dump(a);
    build(a);
    a.here("star_disc");
    a.jsr("select_dfs");
    a.lda(imm(0));
    a.rts();
}

/// `utility_open`: the name at (&F2),Y opened through OSFIND in mode A, its
/// handle in HANDLE, Not found if it is not there. `utility_close` closes
/// it. `line_number`: the BCD line number in four digits and a space, then
/// one on.
fn opening(a: &mut Asm) {
    a.here("utility_open");
    a.pha();
    a.tya();
    a.clc();
    a.adc(zp(os::TEXT));
    a.tax();
    a.lda(zp(os::TEXT + 1));
    a.adc(imm(0));
    a.tay();
    a.pla();
    a.jsr(entry::OSFIND);
    a.sta(zp(u::HANDLE));
    a.tay();
    far(a, Asm::bne, "err_not_found");
    a.lda(imm(0));
    a.sta(zp(u::LINE));
    a.sta(zp(u::LINE + 1));
    a.lda(imm(1));
    a.sta(zp(u::AT_START));
    a.rts();

    a.here("utility_close");
    a.lda(imm(0));
    a.ldy(zp(u::HANDLE));
    a.jmp(abs(entry::OSFIND));

    a.here("line_number");
    a.sed();
    a.lda(zp(u::LINE));
    a.clc();
    a.adc(imm(1));
    a.sta(zp(u::LINE));
    a.lda(zp(u::LINE + 1));
    a.adc(imm(0));
    a.sta(zp(u::LINE + 1));
    a.cld();
    a.jsr("print_hex");
    a.lda(zp(u::LINE));
    a.jsr("print_hex");
    a.jmp("print_space");

    // An Escape pending ends the utility: its file closed, then the error.
    a.here("utility_escape");
    a.bit(zp(ESCAPE_FLAG));
    a.bpl(RTS);
    a.jsr("utility_close");
    a.jmp("escape");
}

/// *TYPE and *LIST: the file's characters, a carriage return a new line,
/// *LIST numbering each line as it starts.
fn type_and_list(a: &mut Asm) {
    a.here("star_type");
    a.lda(imm(0));
    let start = a.local();
    a.beq(start);
    a.here("star_list");
    a.lda(imm(0xFF));
    a.bind(start);
    a.sta(zp(u::ENDED));
    a.lda(imm(OPEN_IN));
    a.jsr("utility_open");
    let next = a.here_local();
    a.jsr("utility_escape");
    a.ldy(zp(u::HANDLE));
    a.jsr(entry::OSBGET);
    let done = a.local();
    a.bcs(done);
    a.pha();
    a.lda(zp(u::AT_START));
    let within = a.local();
    a.beq(within);
    a.lda(imm(0));
    a.sta(zp(u::AT_START));
    a.bit(zp(u::ENDED));
    a.bpl(within);
    a.jsr("line_number");
    a.bind(within);
    a.pla();
    a.cmp(imm(CR));
    let shown = a.local();
    a.bne(shown);
    a.inc(zp(u::AT_START));
    a.bind(shown);
    a.jsr(entry::OSASCI);
    a.jmp(next);
    a.bind(done);
    a.jsr(entry::OSNEWL);
    a.jmp("utility_close");
}

/// *DUMP: rows of the offset in four hex digits, eight bytes in hex, then
/// them as characters, `.` for one not printable; until a row meets the
/// end.
fn dump(a: &mut Asm) {
    a.here("star_dump");
    a.lda(imm(OPEN_IN));
    a.jsr("utility_open");
    a.lda(imm(0));
    a.sta(zp(u::OFFSET));
    a.sta(zp(u::OFFSET + 1));
    a.sta(zp(u::ENDED));
    let row = a.here_local();
    a.jsr("utility_escape");
    a.lda(zp(u::OFFSET + 1));
    a.jsr("print_hex");
    a.lda(zp(u::OFFSET));
    a.jsr("print_hex");
    a.jsr("print_space");
    a.ldx(imm(0));
    let byte = a.here_local();
    a.stx(zp(u::COUNT));
    a.lda(zp(u::ENDED));
    let past = a.local();
    a.bne(past);
    a.ldy(zp(u::HANDLE));
    a.jsr(entry::OSBGET);
    let read = a.local();
    a.bcc(read);
    a.dec(zp(u::ENDED));
    a.bind(past);
    a.lda(imm(PAST));
    a.jsr(entry::OSASCI);
    a.jsr(entry::OSASCI);
    a.lda(imm(b'.'));
    let stored = a.local();
    a.bne(stored);
    a.bind(read);
    a.pha();
    a.jsr("print_hex");
    a.pla();
    a.cmp(imm(FIRST_SHOWN));
    let dot = a.local();
    a.bcc(dot);
    a.cmp(imm(LAST_SHOWN + 1));
    a.bcc(stored);
    a.bind(dot);
    a.lda(imm(b'.'));
    a.bind(stored);
    a.ldx(zp(u::COUNT));
    a.sta(abs(page::WORK).x());
    a.jsr("print_space");
    a.ldx(zp(u::COUNT));
    a.inx();
    a.cpx(imm(ROW));
    a.bne(byte);
    a.ldx(imm(0));
    let shown = a.here_local();
    a.lda(abs(page::WORK).x());
    a.jsr(entry::OSASCI);
    a.inx();
    a.cpx(imm(ROW));
    a.bne(shown);
    a.jsr(entry::OSNEWL);
    a.lda(zp(u::OFFSET));
    a.clc();
    a.adc(imm(ROW));
    a.sta(zp(u::OFFSET));
    let same = a.local();
    a.bcc(same);
    a.inc(zp(u::OFFSET + 1));
    a.bind(same);
    a.lda(zp(u::ENDED));
    a.beq(row);
    a.jmp("utility_close");
}

/// *BUILD: each line typed after its number written with a carriage
/// return, until Escape closes the file. The line is read into the second
/// private page.
fn build(a: &mut Asm) {
    a.here("star_build");
    a.lda(imm(OPEN_OUT));
    a.jsr("utility_open");
    let line = a.here_local();
    a.jsr("line_number");
    // OSWORD 0's block: the buffer, its length and the characters taken.
    a.ldx(zp(os::ROM));
    a.lda(abs(page_d::PRIVATE_WORKSPACE).x());
    a.and(imm(!private::OWNED));
    a.clc();
    a.adc(imm(1));
    a.sta(abs(page::WORK + 1));
    a.sta(zp(z::POINTER + 1));
    a.lda(imm(0));
    a.sta(abs(page::WORK));
    a.sta(zp(z::POINTER));
    a.lda(imm(LINE_MAX));
    a.sta(abs(page::WORK + 2));
    a.lda(imm(LINE_LOW));
    a.sta(abs(page::WORK + 3));
    a.lda(imm(LINE_HIGH));
    a.sta(abs(page::WORK + 4));
    a.lda(imm(0));
    a.ldx(imm(crate::rom::asm::lo(page::WORK)));
    a.ldy(imm(crate::rom::asm::hi(page::WORK)));
    a.jsr(entry::OSWORD);
    let escaped = a.local();
    a.bcs(escaped);
    // Y bytes typed, then the carriage return.
    a.sty(zp(u::COUNT));
    a.ldy(imm(0));
    let write = a.here_local();
    a.lda(zp(z::POINTER).ind_y());
    a.sty(zp(u::OFFSET));
    a.ldy(zp(u::HANDLE));
    a.jsr(entry::OSBPUT);
    a.ldy(zp(u::OFFSET));
    a.cpy(zp(u::COUNT));
    a.beq(line);
    a.iny();
    a.bne(write);
    a.bind(escaped);
    a.jsr("utility_close");
    a.jmp("escape");
}
