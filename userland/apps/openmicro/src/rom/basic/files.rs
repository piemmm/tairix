//! Files through the MOS: OPENIN, OPENOUT and OPENUP; CLOSE#, BGET#,
//! BPUT#, EOF#, EXT# and PTR#; and PRINT# and INPUT#, whose records are a
//! type byte then the value's bytes from its last: &40 and an integer, &FF
//! and a packed real, or 0, a length and the text.

use super::float::PACKED;
use super::ws::{kind, page, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, zp, Asm};
use crate::rom::os::{entry, osbyte};

const CR: u8 = 0x0D;
const OSARGS_READ_PTR: u8 = 0;
const OSARGS_WRITE_PTR: u8 = 1;
const OSARGS_EXTENT: u8 = 2;
/// A real's packed bytes while they are written or read.
const PACKED_AT: u16 = page::TEMPS;

pub fn emit(a: &mut Asm) {
    channels(a);
    records(a);
    inputs(a);
}

fn channels(a: &mut Asm) {
    // `#` and the channel's factor: Y the channel. Missing # without it.
    a.here("channel");
    a.jsr("next_a");
    a.cmp(imm(b'#'));
    let hash = a.local();
    a.beq(hash);
    a.jmp("err_missing_hash");
    a.bind(hash);
    a.jsr("factor_integer");
    a.dec(zp(z::OFFSET_A));
    a.ldy(zp(z::IAC));
    a.rts();

    // OPENIN, OPENOUT and OPENUP: the file's name to OSFIND, its handle
    // returned.
    let open = a.local();
    a.here("fn_openin");
    a.lda(imm(0x40));
    a.bne(open);
    a.here("fn_openout");
    a.lda(imm(0x80));
    a.bne(open);
    a.here("fn_openup");
    a.lda(imm(0xC0));
    a.bind(open);
    a.pha();
    a.jsr("arg_string");
    a.ldy(zp(z::STRING_LENGTH));
    a.lda(imm(CR));
    a.sta(abs(page::STRING).y());
    a.pla();
    a.ldx(imm(lo(page::STRING)));
    a.ldy(imm(hi(page::STRING)));
    a.jsr(entry::OSFIND);
    a.jmp("return_a");

    // CLOSE#c.
    a.here("statement_close");
    a.jsr("channel");
    a.lda(imm(0));
    a.jsr(entry::OSFIND);
    a.jmp("statement_done");

    // BGET#c and BPUT#c, n.
    a.here("fn_bget");
    a.jsr("channel");
    a.jsr(entry::OSBGET);
    a.jmp("return_a");
    a.here("statement_bput");
    a.jsr("channel");
    a.tya();
    a.pha();
    a.jsr("next_a");
    a.tax();
    a.jsr("comma");
    a.jsr("expr_integer");
    a.pla();
    a.tay();
    a.lda(zp(z::IAC));
    a.jsr(entry::OSBPUT);
    a.jmp("statement_done_x");

    // EOF#c: TRUE at the end.
    a.here("fn_eof");
    a.jsr("channel");
    a.tya();
    a.tax();
    a.lda(imm(osbyte::EOF));
    a.jsr(entry::OSBYTE);
    a.txa();
    let open_ = a.local();
    a.beq(open_);
    a.lda(imm(0xFF));
    a.bind(open_);
    a.jmp("return_signed");

    // EXT#c and PTR#c: OSARGS into IAC.
    a.here("fn_ext");
    a.lda(imm(OSARGS_EXTENT));
    a.bne("osargs_read");
    a.here("fn_ptr");
    a.lda(imm(OSARGS_READ_PTR));
    a.here("osargs_read");
    a.pha();
    a.jsr("channel");
    a.pla();
    a.ldx(imm(lo(z::IAC)));
    a.jsr(entry::OSARGS);
    a.lda(imm(kind::INTEGER));
    a.rts();

    // PTR#c = n.
    a.here("statement_ptr");
    a.jsr("channel");
    a.tya();
    a.pha();
    a.jsr("pseudo_value");
    a.pla();
    a.tay();
    a.lda(imm(OSARGS_WRITE_PTR));
    a.ldx(imm(lo(z::IAC)));
    a.jsr(entry::OSARGS);
    a.jmp("statement_ended");
}

fn records(a: &mut Asm) {
    // PRINT#c, items: each a record. The channel waits on the 6502 stack.
    a.here("print_file");
    a.jsr("channel");
    a.tya();
    a.pha();
    let (item, done) = (a.here_local(), a.local());
    a.jsr("next_a");
    a.cmp(imm(b','));
    a.bne(done);
    a.jsr("expr");
    a.stx(zp(z::OP));
    a.tax();
    a.pla();
    a.pha();
    a.tay();
    a.txa();
    let (string, real, sent) = (a.local(), a.local(), a.local());
    a.beq(string);
    a.bmi(real);
    a.jsr(entry::OSBPUT);
    a.ldx(imm(4));
    let byte = a.here_local();
    a.lda(abs(z::IAC - 1).x());
    a.jsr(entry::OSBPUT);
    a.dex();
    a.bne(byte);
    a.beq(sent);
    a.bind(real);
    a.jsr(entry::OSBPUT);
    a.lda(imm(lo(PACKED_AT)));
    a.sta(zp(z::VAR));
    a.lda(imm(hi(PACKED_AT)));
    a.sta(zp(z::VAR + 1));
    a.jsr("fac_store");
    a.pla();
    a.pha();
    a.tay();
    a.ldx(imm(PACKED));
    let byte = a.here_local();
    a.lda(abs(PACKED_AT - 1).x());
    a.jsr(entry::OSBPUT);
    a.dex();
    a.bne(byte);
    a.beq(sent);
    a.bind(string);
    a.jsr(entry::OSBPUT);
    a.lda(zp(z::STRING_LENGTH));
    a.jsr(entry::OSBPUT);
    a.ldx(zp(z::STRING_LENGTH));
    let text = a.here_local();
    a.beq(sent);
    a.lda(abs(page::STRING - 1).x());
    a.jsr(entry::OSBPUT);
    a.dex();
    a.jmp(text);
    a.bind(sent);
    a.ldx(zp(z::OP));
    a.dec(zp(z::OFFSET_A));
    a.jmp(item);
    a.bind(done);
    a.tax();
    a.pla();
    a.jmp("statement_done_x");
}

fn inputs(a: &mut Asm) {
    // INPUT#c, locations: each from a record, numbers converting.
    a.here("input_file");
    a.jsr("channel");
    a.tya();
    a.pha();
    let (item, done) = (a.here_local(), a.local());
    a.jsr("next_a");
    a.cmp(imm(b','));
    a.bne(done);
    a.jsr("lvalue");
    let located = a.local();
    a.bcc(located);
    a.jmp("err_mistake");
    a.bind(located);
    a.pla();
    a.pha();
    a.tay();
    a.jsr(entry::OSBGET);
    let (string, real, fetched) = (a.local(), a.local(), a.local());
    a.tax();
    a.beq(string);
    a.bmi(real);
    a.ldx(imm(4));
    let byte = a.here_local();
    a.jsr(entry::OSBGET);
    a.sta(abs(z::IAC - 1).x());
    a.dex();
    a.bne(byte);
    a.lda(imm(kind::INTEGER));
    a.bne(fetched);
    a.bind(real);
    a.lda(zp(z::VAR));
    a.pha();
    a.lda(zp(z::VAR + 1));
    a.pha();
    a.ldx(imm(PACKED));
    let byte = a.here_local();
    a.jsr(entry::OSBGET);
    a.sta(abs(PACKED_AT - 1).x());
    a.dex();
    a.bne(byte);
    a.lda(imm(lo(PACKED_AT)));
    a.sta(zp(z::VAR));
    a.lda(imm(hi(PACKED_AT)));
    a.sta(zp(z::VAR + 1));
    a.jsr("fac_load");
    a.pla();
    a.sta(zp(z::VAR + 1));
    a.pla();
    a.sta(zp(z::VAR));
    a.lda(imm(kind::REAL));
    a.bne(fetched);
    a.bind(string);
    a.jsr(entry::OSBGET);
    a.sta(zp(z::STRING_LENGTH));
    a.tax();
    let text = a.here_local();
    let texted = a.local();
    a.beq(texted);
    a.jsr(entry::OSBGET);
    a.sta(abs(page::STRING - 1).x());
    a.dex();
    a.jmp(text);
    a.bind(texted);
    a.lda(imm(kind::STRING));
    a.bind(fetched);
    a.jsr("store_variable");
    a.jmp(item);
    a.bind(done);
    a.tax();
    a.pla();
    a.jmp("statement_done_x");
}
