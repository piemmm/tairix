//! Printing, and reading names and arguments from a command line at (&F2)
//! through GSREAD, so quotes and `|` escapes work as in every command.
//!
//! A file name is `[:drive.][dir.]name`: a later `dir.` replaces an
//! earlier, and the name is at most seven characters. `*` and `#` are kept
//! as characters, and mark the name as wild.

use super::far;
use super::ws::{page, zp as z};
use crate::rom::asm::{abs, imm, zp, Acc, Asm, RTS};
use crate::rom::os::{entry, zp as os};

const CR: u8 = 0x0D;
/// The name's length and the wildcards.
pub(super) const NAME_LEN: u8 = 7;
pub(super) const MANY: u8 = b'*';
pub(super) const ONE: u8 = b'#';
/// The drives there are.
pub(super) const DRIVES: u8 = 4;
const _: () = assert!(
    z::DIR == z::NAME + NAME_LEN as u16,
    "the directory follows the name"
);

pub(super) fn emit(a: &mut Asm) {
    printing(a);
    names(a);
    arguments(a);
}

fn printing(a: &mut Asm) {
    // The text after the JSR, to its zero; the caller resumes past it.
    a.here("print_inline");
    a.pla();
    a.sta(zp(z::PRINT));
    a.pla();
    a.sta(zp(z::PRINT + 1));
    a.tya();
    a.pha();
    a.ldy(imm(0));
    let next = a.here_local();
    a.inc(zp(z::PRINT));
    let same = a.local();
    a.bne(same);
    a.inc(zp(z::PRINT + 1));
    a.bind(same);
    a.lda(zp(z::PRINT).ind_y());
    let done = a.local();
    a.beq(done);
    a.jsr(entry::OSASCI);
    a.jmp(next);
    a.bind(done);
    a.pla();
    a.tay();
    a.lda(zp(z::PRINT + 1));
    a.pha();
    a.lda(zp(z::PRINT));
    a.pha();
    a.rts();

    // A as two hex digits, X and Y kept.
    a.here("print_hex");
    a.pha();
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.jsr("print_digit");
    a.pla();
    a.here("print_digit");
    a.jsr("hex_digit");
    a.jmp(abs(entry::OSASCI));

    // A's low nibble as its hex digit.
    a.here("hex_digit");
    a.and(imm(0x0F));
    a.cmp(imm(10));
    let decimal = a.local();
    a.bcc(decimal);
    a.adc(imm(b'A' - b'0' - 10 - 1));
    a.bind(decimal);
    a.adc(imm(b'0'));
    a.rts();

    // X spaces.
    a.here("print_spaces");
    a.jsr("print_space");
    a.dex();
    a.bne("print_spaces");
    a.rts();
    a.here("print_space");
    a.lda(imm(b' '));
    a.jmp(abs(entry::OSASCI));
}

/// `parse_name`: the name at (&F2),Y into NAME, DIR, DRIVE and WILD, Y left
/// at the word after it.
fn names(a: &mut Asm) {
    a.here("parse_name");
    a.lda(abs(page::DIR));
    a.sta(zp(z::DIR));
    a.lda(abs(page::DRIVE));
    a.sta(zp(z::DRIVE));
    a.lda(imm(0));
    a.sta(abs(page::QUALIFIED));
    a.ldx(imm(NAME_LEN - 1));
    a.lda(imm(b' '));
    let blank = a.here_local();
    a.sta(zp(z::NAME).x());
    a.dex();
    a.bpl(blank);
    a.clc();
    a.jsr(entry::GSINIT);
    a.jsr("name_char");
    a.cmp(imm(b':'));
    let directory = a.local();
    a.bne(directory);
    a.jsr("name_char");
    a.jsr("drive_digit");
    a.sta(zp(z::DRIVE));
    a.inc(abs(page::QUALIFIED));
    a.jsr(entry::GSREAD);
    far(a, Asm::bcc, "err_bad_name");
    a.cmp(imm(b'.'));
    far(a, Asm::beq, "err_bad_drive");
    a.jsr("name_char");
    // A directory is a character before a full stop, the last one winning.
    a.bind(directory);
    a.sta(zp(z::T0));
    a.jsr(entry::GSREAD);
    let single = a.local();
    a.bcs(single);
    a.cmp(imm(b'.'));
    let name = a.local();
    a.bne(name);
    a.lda(zp(z::T0));
    a.sta(zp(z::DIR));
    a.sta(abs(page::QUALIFIED));
    a.jsr("name_char");
    a.jmp(directory);
    a.bind(single);
    a.lda(zp(z::T0));
    a.sta(zp(z::NAME));
    a.jmp("name_wild");
    a.bind(name);
    a.ldx(zp(z::T0));
    a.stx(zp(z::NAME));
    a.ldx(imm(1));
    let store = a.here_local();
    a.cpx(imm(NAME_LEN));
    far(a, Asm::bne, "err_bad_name");
    a.sta(zp(z::NAME).x());
    a.inx();
    a.jsr(entry::GSREAD);
    a.bcc(store);
    // WILD set if a wildcard is in the name or is the directory, which
    // follows it.
    a.here("name_wild");
    a.ldx(imm(NAME_LEN));
    let scan = a.here_local();
    a.lda(zp(z::NAME).x());
    a.jsr("is_wild");
    let wild = a.local();
    a.beq(wild);
    a.dex();
    a.bpl(scan);
    a.lda(imm(0));
    let set = a.local();
    a.beq(set);
    a.bind(wild);
    a.lda(imm(0xFF));
    a.bind(set);
    a.sta(zp(z::WILD));
    a.rts();

    // Z set if A is a wildcard.
    a.here("is_wild");
    a.cmp(imm(MANY));
    a.beq(RTS);
    a.cmp(imm(ONE));
    a.rts();

    // The name's next character, there being one.
    a.here("name_char");
    a.jsr(entry::GSREAD);
    far(a, Asm::bcc, "err_bad_name");
    a.rts();

    // A drive number from its digit, which must name one of the four.
    a.here("drive_digit");
    a.sec();
    a.sbc(imm(b'0'));
    a.cmp(imm(DRIVES));
    far(a, Asm::bcc, "err_bad_drive");
    a.rts();
}

/// `next_word`: A the character at (&F2),Y past spaces, Z set if it ends the
/// line. `drive_argument`: a drive, `:` before it optional, into A with C
/// clear, or C set if the line has ended. `dir_argument`: `[:drive.]dir` or
/// `:drive` into DIR and DRIVE.
fn arguments(a: &mut Asm) {
    a.here("next_word");
    let again = a.here_local();
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(b' '));
    let word = a.local();
    a.bne(word);
    a.iny();
    a.bne(again);
    a.bind(word);
    a.cmp(imm(CR));
    a.rts();

    a.here("drive_argument");
    a.jsr("next_word");
    a.sec();
    a.beq(RTS);
    a.cmp(imm(b':'));
    let digit = a.local();
    a.bne(digit);
    a.iny();
    a.lda(zp(os::TEXT).ind_y());
    a.bind(digit);
    a.jsr("drive_digit");
    a.pha();
    a.iny();
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(b' '));
    let ended = a.local();
    a.beq(ended);
    a.cmp(imm(CR));
    far(a, Asm::beq, "err_bad_drive");
    a.bind(ended);
    a.jsr("next_word");
    a.pla();
    a.clc();
    a.rts();

    a.here("dir_argument");
    a.lda(abs(page::DRIVE));
    a.sta(zp(z::DRIVE));
    a.clc();
    a.jsr(entry::GSINIT);
    a.jsr(entry::GSREAD);
    far(a, Asm::bcc, "err_bad_dir");
    a.cmp(imm(b':'));
    let plain = a.local();
    a.bne(plain);
    a.jsr(entry::GSREAD);
    far(a, Asm::bcc, "err_bad_drive");
    a.jsr("drive_digit");
    a.sta(zp(z::DRIVE));
    a.jsr(entry::GSREAD);
    a.bcs(RTS);
    a.cmp(imm(b'.'));
    far(a, Asm::beq, "err_bad_drive");
    a.jsr(entry::GSREAD);
    far(a, Asm::bcc, "err_bad_dir");
    a.bind(plain);
    a.sta(zp(z::DIR));
    a.jsr(entry::GSREAD);
    far(a, Asm::bcs, "err_bad_dir");
    a.rts();
}
