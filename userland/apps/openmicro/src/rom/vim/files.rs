//! Files: one read into the text by its kind — a BASIC program listed, a
//! text's lines ending as it ends them — and written back as it was read.
//! A file is loaded against the register, the top of the room the text
//! has, and decoded down into the text from OSHWM.

use super::ops::{add16, cmp16, dec16, inc16, mov16, sub16, zero16};
use super::ws::{file, page, reading, show, syntax, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, zp, Asm, RTS};
use crate::rom::os::{entry, CR};

/// The line feed.
const LF: u8 = 0x0A;
/// OSFILE's calls.
const READ_INFO: u8 = 5;
const WRITE_INFO: u8 = 1;
const LOAD: u8 = 0xFF;
const SAVE: u8 = 0;
/// OSFIND's call that opens a file to write.
const OPEN_OUT: u8 = 0x80;
/// OSGBPB's call that writes bytes at the file's pointer, and its block,
/// laid over OSFILE's after the name, which a part written leaves for the
/// file's addresses to be given by OSFILE 1: the handle, the address, the
/// count, then the pointer.
const WRITE_BYTES: u8 = 2;
const GBPB_HANDLE: u16 = page::BLOCK + 2;
const GBPB_ADDRESS: u16 = GBPB_HANDLE + 1;
const GBPB_COUNT: u16 = GBPB_ADDRESS + 4;
const _: () = assert!(GBPB_COUNT + 8 <= page::BLOCK + 18);
/// What OSFILE 5 finds.
const NOT_FOUND: u8 = 0;
const A_FILE: u8 = 1;
/// The offsets in OSFILE's block.
const BLOCK_LOAD: u16 = page::BLOCK + 2;
const BLOCK_EXEC: u16 = page::BLOCK + 6;
const BLOCK_START: u16 = page::BLOCK + 10;
const BLOCK_END: u16 = page::BLOCK + 14;
const BLOCK_ATTRIBUTES: u16 = page::BLOCK + 14;
/// The high bytes of an address in this machine's own memory.
const HERE: u8 = 0xFF;
/// The name whose text is a boot file's statements.
const BOOT: &[u8] = b"!BOOT";

pub(super) fn emit(a: &mut Asm) {
    opening(a);
    decoding(a);
    recognising(a);
    text_in(a);
    writing(a);
    emitting(a);
    reporting(a);
    reading(a);
    output(a);
    flushing(a);
    saving(a);
    naming(a);
}

/// `open_file`: the file the name names read into an emptied text, or a new
/// file where there is none; the text left empty and the name dropped where
/// it cannot be read, so nothing writes over what is there.
fn opening(a: &mut Asm) {
    a.here("open_file");
    a.jsr("buf_empty");
    a.lda(imm(reading::LOADING));
    a.sta(zp(z::READING));
    a.lda(imm(0));
    a.sta(zp(z::FILE));
    a.sta(zp(z::SYN));
    a.ldx(imm(7));
    let clear = a.here_local();
    a.sta(abs(page::LOAD).x());
    a.dex();
    a.bpl(clear);
    a.jsr("name_block");
    a.lda(imm(READ_INFO));
    a.jsr("osfile");
    a.cmp(imm(A_FILE));
    let a_file = a.local();
    a.beq(a_file);
    a.cmp(imm(NOT_FOUND));
    let unreadable = a.local();
    a.bne(unreadable);
    a.lda(imm(file::NEW));
    a.sta(zp(z::FILE));
    a.lda(imm(reading::OPENED));
    a.sta(zp(z::READING));
    a.jsr("name_syntax");
    a.jsr("msg_begin");
    a.jsr("msg_name");
    a.call_with_text("msg_text", &[" [New]"]);
    a.jsr("msg_end");
    let opened = a.local();
    a.jmp(opened);
    a.bind(unreadable);
    a.jsr("say_not_file");
    a.jmp("drop_name");
    let too_big = a.local();
    a.bind(too_big);
    a.jsr("say_too_big");
    a.jmp("drop_name");
    // Too long: past 64K, or past the room the text has.
    a.bind(a_file);
    a.lda(abs(BLOCK_START + 2));
    a.ora(abs(BLOCK_START + 3));
    a.bne(too_big);
    a.lda(abs(BLOCK_START));
    a.sta(zp(z::N1));
    a.lda(abs(BLOCK_START + 1));
    a.sta(zp(z::N1 + 1));
    sub16(a, z::N2, z::REG, z::BUF);
    cmp16(a, z::N2, z::N1);
    a.bcc(too_big);
    // Its addresses kept for writing it back; it is loaded against the
    // register.
    a.ldx(imm(7));
    let keep = a.here_local();
    a.lda(abs(BLOCK_LOAD).x());
    a.sta(abs(page::LOAD).x());
    a.dex();
    a.bpl(keep);
    sub16(a, z::BASIC_IN, z::REG, z::N1);
    a.lda(zp(z::BASIC_IN));
    a.sta(abs(BLOCK_LOAD));
    a.lda(zp(z::BASIC_IN + 1));
    a.sta(abs(BLOCK_LOAD + 1));
    a.lda(imm(HERE));
    a.sta(abs(BLOCK_LOAD + 2));
    a.sta(abs(BLOCK_LOAD + 3));
    a.lda(imm(0));
    a.sta(abs(BLOCK_EXEC));
    a.lda(imm(LOAD));
    a.jsr("osfile");
    a.lda(imm(reading::OPENED));
    a.sta(zp(z::READING));
    a.jsr("decode_in");
    a.jsr("name_syntax");
    a.jsr("cmd_info");
    a.bind(opened);
    a.lda(imm(0));
    a.sta(zp(z::N0));
    a.sta(zp(z::N0 + 1));
    a.sta(zp(z::WANT));
    a.jsr("set_cursor");
    a.lda(imm(show::ALL));
    a.jmp("owe");
}

/// Decoding a file read into the text.
fn decoding(a: &mut Asm) {
    // `decode_in`: the N1 bytes at BASIC_IN put into the text at the gap: a
    // BASIC program listed, a text's lines ended in CR.
    a.here("decode_in");
    mov16(a, z::BASIC_OUT, z::GAP);
    a.jsr("is_program");
    let program = a.local();
    a.bcs(program);
    a.jmp("text_in");
    a.bind(program);
    a.lda(zp(z::FILE));
    a.ora(imm(file::BASIC));
    a.sta(zp(z::FILE));
    a.jmp("basic_list");

    // `decode_failed`: a file that cannot be decoded, already said why: one
    // being read into the text leaves it as it was, one being opened
    // leaves it empty and nameless.
    a.here("decode_failed");
    a.lda(zp(z::READING));
    crate::rom::far(a, Asm::beq, "main");
    a.fall_into("drop_name");

    // `drop_name`: an emptied text with no name, back to the main loop.
    a.here("drop_name");
    a.jsr("buf_empty");
    a.lda(imm(0));
    a.sta(zp(z::FILE));
    a.sta(zp(z::READING));
    a.lda(imm(CR));
    a.sta(abs(page::NAME));
    a.lda(imm(show::ALL));
    a.jsr("owe");
    a.jmp("main");

    // `decoded`: what was written from the gap to BASIC_OUT taken into the
    // text, the gap after it.
    a.here("decoded");
    sub16(a, z::N2, z::BASIC_OUT, z::GAP);
    mov16(a, z::GAP, z::BASIC_OUT);
    add16(a, z::GOFF, z::GOFF, z::N2);
    add16(a, z::LEN, z::LEN, z::N2);
    a.rts();

    // `name_block`: OSFILE's block naming the file's own name.
    a.here("name_block");
    a.lda(imm(lo(page::NAME)));
    a.sta(abs(page::BLOCK));
    a.lda(imm(hi(page::NAME)));
    a.sta(abs(page::BLOCK + 1));
    a.rts();

    // `osfile`: OSFILE A on the block.
    a.here("osfile");
    a.ldx(imm(lo(page::BLOCK)));
    a.ldy(imm(hi(page::BLOCK)));
    a.jmp(abs(entry::OSFILE));
}

/// `is_program`: whether a file is a BASIC program.
fn recognising(a: &mut Asm) {
    // `is_program`: carry set where the N1 bytes from BASIC_IN are a BASIC
    // program: lines each a CR, a number below &8000 and a length that
    // reaches the next, to a CR and &FF.
    a.here("is_program");
    mov16(a, z::P0, z::BASIC_IN);
    mov16(a, z::N2, z::N1);
    let line = a.here_local();
    let no = a.local();
    let yes = a.local();
    a.lda(zp(z::N2 + 1));
    let long = a.local();
    a.bne(long);
    a.lda(zp(z::N2));
    a.cmp(imm(2));
    a.bcc(no);
    a.bind(long);
    a.ldy(imm(0));
    a.lda(zp(z::P0).ind_y());
    a.cmp(imm(CR));
    a.bne(no);
    a.iny();
    a.lda(zp(z::P0).ind_y());
    a.cmp(imm(0xFF));
    a.beq(yes);
    a.and(imm(0x80));
    a.bne(no);
    a.lda(zp(z::N2 + 1));
    let room = a.local();
    a.bne(room);
    a.lda(zp(z::N2));
    a.cmp(imm(4));
    a.bcc(no);
    a.bind(room);
    a.ldy(imm(3));
    a.lda(zp(z::P0).ind_y());
    a.cmp(imm(4));
    a.bcc(no);
    a.sta(zp(z::T0));
    a.lda(zp(z::N2 + 1));
    let within = a.local();
    a.bne(within);
    a.lda(zp(z::N2));
    a.cmp(zp(z::T0));
    a.bcc(no);
    a.bind(within);
    a.lda(zp(z::P0));
    a.clc();
    a.adc(zp(z::T0));
    a.sta(zp(z::P0));
    let carried = a.local();
    a.bcc(carried);
    a.inc(zp(z::P0 + 1));
    a.bind(carried);
    a.lda(zp(z::N2));
    a.sec();
    a.sbc(zp(z::T0));
    a.sta(zp(z::N2));
    let borrowed = a.local();
    a.bcs(borrowed);
    a.dec(zp(z::N2 + 1));
    a.bind(borrowed);
    a.jmp(line);
    a.bind(yes);
    a.sec();
    a.rts();
    a.bind(no);
    a.clc();
    a.rts();
}

/// `text_in`: the N1 bytes from `BASIC_IN` as text from `BASIC_OUT`: each line
/// ends in CR, its end in the file noted — CR, LF, CR LF or LF CR — for
/// writing it back. A line past 255 characters refuses the file.
fn text_in(a: &mut Asm) {
    a.here("text_in");
    a.lda(imm(0));
    a.sta(zp(z::T1));
    a.sta(zp(z::T2));
    a.sta(zp(z::T3));
    let next = a.here_local();
    let finished = a.local();
    zero16(a, z::N1);
    a.beq(finished);
    a.ldy(imm(0));
    a.lda(zp(z::BASIC_IN).ind_y());
    a.tax();
    inc16(a, z::BASIC_IN);
    dec16(a, z::N1);
    a.txa();
    a.cmp(imm(CR));
    let an_end = a.local();
    a.beq(an_end);
    a.cmp(imm(LF));
    a.beq(an_end);
    // A character: an LF waiting stood alone.
    a.ldx(zp(z::T1));
    a.cpx(imm(LF));
    let not_lone = a.local();
    a.bne(not_lone);
    a.pha();
    a.jsr("lone_lf");
    a.pla();
    a.bind(not_lone);
    a.ldx(imm(0));
    a.stx(zp(z::T1));
    a.inc(zp(z::T2));
    let too_long = a.local();
    a.beq(too_long);
    a.jsr("text_out");
    a.jmp(next);
    // A line's end: the second of a pair, or one of its own.
    a.bind(an_end);
    a.ldx(zp(z::T1));
    let first_end = a.local();
    a.beq(first_end);
    a.stx(zp(z::T4));
    a.cmp(zp(z::T4));
    let same_again = a.local();
    a.beq(same_again);
    a.cpx(imm(CR));
    let lf_cr = a.local();
    a.bne(lf_cr);
    a.lda(imm(file::CRLF));
    let paired = a.local();
    a.bne(paired);
    a.bind(lf_cr);
    a.lda(imm(file::LFCR));
    a.bind(paired);
    a.ora(zp(z::T3));
    a.sta(zp(z::T3));
    a.lda(imm(0));
    a.sta(zp(z::T1));
    a.jmp(next);
    a.bind(same_again);
    a.cpx(imm(LF));
    a.bne(first_end);
    a.jsr("lone_lf");
    a.lda(imm(LF));
    a.bind(first_end);
    a.sta(zp(z::T1));
    a.lda(imm(0));
    a.sta(zp(z::T2));
    a.lda(imm(CR));
    a.jsr("text_out");
    a.jmp(next);
    // The file's ends, as found: a pair before an LF alone.
    a.bind(finished);
    a.lda(zp(z::T1));
    a.cmp(imm(LF));
    let kinds = a.local();
    a.bne(kinds);
    a.jsr("lone_lf");
    a.bind(kinds);
    for kind in [file::CRLF, file::LFCR, file::LF] {
        a.lda(zp(z::T3));
        a.and(imm(kind));
        let other = a.local();
        a.beq(other);
        a.ora(zp(z::FILE));
        a.sta(zp(z::FILE));
        a.jmp("decoded");
        a.bind(other);
    }
    a.jmp("decoded");
    a.bind(too_long);
    a.jsr("say_too_long");
    a.jmp("decode_failed");

    a.here("lone_lf");
    a.lda(zp(z::T3));
    a.ora(imm(file::LF));
    a.sta(zp(z::T3));
    a.rts();

    a.here("text_out");
    a.ldy(imm(0));
    a.sta(zp(z::BASIC_OUT).ind_y());
    inc16(a, z::BASIC_OUT);
    a.rts();
}

/// `write_file`: the text written to the file OSFILE's block names, in its
/// kind: tokenised for a BASIC program, its lines' ends as the file had
/// them for a text, a last line given its CR.
fn writing(a: &mut Asm) {
    a.here("write_file");
    a.lda(zp(z::FILE));
    a.and(imm(file::BASIC));
    let emitted = a.local();
    let text = a.local();
    a.beq(text);
    a.jsr("basic_addresses");
    a.jmp(emitted);
    a.bind(text);
    a.jsr("end_last_line");
    crate::rom::far(a, Asm::bcc, "no_room");
    a.lda(zp(z::FILE));
    a.and(imm(file::CRLF | file::LFCR));
    a.bne(emitted);
    mov16(a, z::P1, z::BUF);
    a.jsr("p2_gap");
    a.lda(zp(z::FILE));
    a.and(imm(file::LF));
    let plain = a.local();
    a.beq(plain);
    a.jsr("swap_ends");
    a.jsr("save_range");
    a.jsr("swap_ends");
    a.jmp("written");
    a.bind(plain);
    a.jsr("save_range");
    a.jmp("written");
    // A program, or a text whose lines end in pairs: built in the gap and
    // saved from it in one call; or, once it outgrows the gap, written to
    // the file opened for it a part at a time, then given its addresses.
    a.bind(emitted);
    a.jsr("n0_len");
    a.jsr("move_gap");
    mov16(a, z::BASIC_OUT, z::GAP);
    mov16(a, z::REND, z::GAP);
    a.jsr("emit");
    a.lda(abs(page::HANDLE));
    let parted = a.local();
    a.bne(parted);
    mov16(a, z::P1, z::GAP);
    mov16(a, z::P2, z::BASIC_OUT);
    a.jsr("save_range");
    a.jmp("written");
    a.bind(parted);
    mov16(a, z::REND, z::BASIC_OUT);
    a.jsr("flush_out");
    a.jsr("close_file");
    a.jsr("block_addresses");
    a.lda(imm(0));
    for at in 0..4 {
        a.sta(abs(BLOCK_ATTRIBUTES + at));
    }
    a.lda(imm(WRITE_INFO));
    a.jsr("osfile");
    a.jmp("written");
}

/// `emit`: the text put out as its kind is written.
fn emitting(a: &mut Asm) {
    // `emit`: the text put out through `out_byte` as its kind is written.
    a.here("emit");
    a.lda(zp(z::FILE));
    a.and(imm(file::BASIC));
    let pairs = a.local();
    a.beq(pairs);
    a.jmp("basic_emit");
    // Each line's end a pair.
    a.bind(pairs);
    a.lda(zp(z::FILE));
    a.and(imm(file::CRLF));
    let lf_first = a.local();
    a.beq(lf_first);
    a.lda(imm(CR));
    a.ldx(imm(LF));
    let chosen = a.local();
    a.bne(chosen);
    a.bind(lf_first);
    a.lda(imm(LF));
    a.ldx(imm(CR));
    a.bind(chosen);
    a.sta(zp(z::T1));
    a.stx(zp(z::T2));
    mov16(a, z::P0, z::BUF);
    let each = a.here_local();
    a.lda(zp(z::P0));
    a.cmp(zp(z::GAP));
    let more = a.local();
    a.bne(more);
    a.lda(zp(z::P0 + 1));
    a.cmp(zp(z::GAP + 1));
    a.beq(RTS);
    a.bind(more);
    a.ldy(imm(0));
    a.lda(zp(z::P0).ind_y());
    inc16(a, z::P0);
    a.cmp(imm(CR));
    let one = a.local();
    a.bne(one);
    a.lda(zp(z::T1));
    a.jsr("out_byte");
    a.lda(zp(z::T2));
    a.bind(one);
    a.jsr("out_byte");
    // A part written keeps nothing back of a whole line.
    mov16(a, z::REND, z::BASIC_OUT);
    a.jmp(each);
}

/// `written`: a write reported.
fn reporting(a: &mut Asm) {
    // `written`: the text no longer changed since its file, and said so.
    a.here("written");
    a.lda(zp(z::FILE));
    a.and(imm(!(file::MODIFIED | file::NEW)));
    a.sta(zp(z::FILE));
    a.jsr("msg_begin");
    a.lda(abs(page::BLOCK));
    a.sta(zp(z::P0));
    a.lda(abs(page::BLOCK + 1));
    a.sta(zp(z::P0 + 1));
    a.jsr("msg_name_at");
    a.call_with_text("msg_text", &[" written"]);
    a.jmp("msg_end");

    // `end_last_line`: the gap moved to the text's end and a CR put there
    // where the last line has none; carry set where there is no room.
    a.here("end_last_line");
    a.jsr("n0_len");
    a.jsr("move_gap");
    zero16(a, z::LEN);
    let ended = a.local();
    a.beq(ended);
    dec16(a, z::N0);
    a.jsr("getc");
    a.cmp(imm(CR));
    a.beq(ended);
    a.lda(zp(z::GAP));
    a.cmp(zp(z::GEND));
    let room = a.local();
    a.bne(room);
    a.lda(zp(z::GAP + 1));
    a.cmp(zp(z::GEND + 1));
    a.bne(room);
    a.sec();
    a.rts();
    a.bind(room);
    a.ldy(imm(0));
    a.lda(imm(CR));
    a.sta(zp(z::GAP).ind_y());
    inc16(a, z::GAP);
    inc16(a, z::GOFF);
    inc16(a, z::LEN);
    a.bind(ended);
    a.clc();
    a.rts();
}

/// `:r`.
fn reading(a: &mut Asm) {
    // `:r name`: the file read in after the cursor's line, as one change.
    a.here("ex_read");
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    crate::rom::far(a, Asm::bne, "no_name");
    a.jsr("arg_end");
    a.lda(zp(z::EX_AT));
    a.clc();
    a.adc(imm(lo(page::INPUT)));
    a.sta(abs(page::BLOCK));
    a.lda(imm(hi(page::INPUT)));
    a.adc(imm(0));
    a.sta(abs(page::BLOCK + 1));
    a.lda(imm(READ_INFO));
    a.jsr("osfile");
    a.cmp(imm(A_FILE));
    let a_file = a.local();
    a.beq(a_file);
    a.jmp("say_not_file");
    a.bind(a_file);
    a.lda(abs(BLOCK_START + 2));
    a.ora(abs(BLOCK_START + 3));
    let short = a.local();
    a.beq(short);
    a.jmp("no_room");
    a.bind(short);
    a.lda(abs(BLOCK_START));
    a.sta(zp(z::N1));
    a.lda(abs(BLOCK_START + 1));
    a.sta(zp(z::N1 + 1));
    // After the cursor's line; after a CR given the last line where that
    // is the cursor's.
    a.jsr("n0_lstart");
    a.jsr("next_line");
    let placed = a.local();
    a.bcc(placed);
    a.jsr("end_last_line");
    crate::rom::far(a, Asm::bcc, "no_room");
    a.jsr("n0_len");
    a.bind(placed);
    a.jsr("begin_change");
    mov16(a, z::HOLD, z::LEN);
    a.jsr("move_gap");
    a.jsr("gap_room");
    crate::rom::far(a, Asm::bcc, "no_room");
    sub16(a, z::BASIC_IN, z::GEND, z::N1);
    a.lda(zp(z::BASIC_IN));
    a.sta(abs(BLOCK_LOAD));
    a.lda(zp(z::BASIC_IN + 1));
    a.sta(abs(BLOCK_LOAD + 1));
    a.lda(imm(HERE));
    a.sta(abs(BLOCK_LOAD + 2));
    a.sta(abs(BLOCK_LOAD + 3));
    a.lda(imm(0));
    a.sta(abs(BLOCK_EXEC));
    a.lda(imm(LOAD));
    a.jsr("osfile");
    a.lda(zp(z::FILE));
    a.pha();
    a.lda(imm(reading::INTO_TEXT));
    a.sta(zp(z::READING));
    a.jsr("decode_in");
    a.pla();
    a.ora(imm(file::MODIFIED));
    a.sta(zp(z::FILE));
    sub16(a, z::UINS, z::LEN, z::HOLD);
    a.jsr("n0_upos");
    a.jmp("land");
}

/// `out_byte`, and the saves the writers end in.
fn output(a: &mut Asm) {
    // `out_byte`: A put at BASIC_OUT in the gap, a part written first where
    // the gap is full; refused where a line in progress fills the gap
    // alone. X and Y kept.
    a.here("out_byte");
    a.sty(zp(z::T4));
    a.pha();
    a.jsr("out_full");
    let free = a.local();
    a.bne(free);
    a.txa();
    a.pha();
    a.jsr("flush_out");
    a.pla();
    a.tax();
    a.jsr("out_full");
    a.bne(free);
    a.jsr("close_file");
    a.jmp("no_room");
    a.bind(free);
    a.pla();
    a.ldy(imm(0));
    a.sta(zp(z::BASIC_OUT).ind_y());
    inc16(a, z::BASIC_OUT);
    a.ldy(zp(z::T4));
    a.rts();

    // `out_full`: zero set where BASIC_OUT has reached the gap's end.
    a.here("out_full");
    a.lda(zp(z::BASIC_OUT));
    a.cmp(zp(z::GEND));
    a.bne(RTS);
    a.lda(zp(z::BASIC_OUT + 1));
    a.cmp(zp(z::GEND + 1));
    a.rts();
}

/// `flush_out`: a part of what is put out written to the file.
fn flushing(a: &mut Asm) {
    // `flush_out`: what is put out before REND written to the file, which
    // is opened for it the first time, and the line in progress from REND
    // moved down to the gap's start. N1 kept.
    a.here("flush_out");
    a.lda(zp(z::N1));
    a.pha();
    a.lda(zp(z::N1 + 1));
    a.pha();
    a.lda(abs(page::HANDLE));
    let opened = a.local();
    a.bne(opened);
    a.ldx(abs(page::BLOCK));
    a.ldy(abs(page::BLOCK + 1));
    a.lda(imm(OPEN_OUT));
    a.jsr(entry::OSFIND);
    a.sta(abs(page::HANDLE));
    a.tay();
    a.bne(opened);
    a.jsr("say_not_file");
    a.jmp("main");
    a.bind(opened);
    a.sta(abs(GBPB_HANDLE));
    sub16(a, z::N1, z::REND, z::GAP);
    for (to, from) in [(GBPB_ADDRESS, z::GAP), (GBPB_COUNT, z::N1)] {
        a.lda(zp(from));
        a.sta(abs(to));
        a.lda(zp(from + 1));
        a.sta(abs(to + 1));
    }
    a.lda(imm(HERE));
    a.sta(abs(GBPB_ADDRESS + 2));
    a.sta(abs(GBPB_ADDRESS + 3));
    a.lda(imm(0));
    a.sta(abs(GBPB_COUNT + 2));
    a.sta(abs(GBPB_COUNT + 3));
    a.lda(imm(WRITE_BYTES));
    a.ldx(imm(lo(GBPB_HANDLE)));
    a.ldy(imm(hi(GBPB_HANDLE)));
    a.jsr(entry::OSGBPB);
    mov16(a, z::P1, z::REND);
    a.jsr("p2_gap");
    sub16(a, z::N1, z::BASIC_OUT, z::REND);
    a.jsr("memmove");
    add16(a, z::BASIC_OUT, z::GAP, z::N1);
    mov16(a, z::REND, z::GAP);
    a.pla();
    a.sta(zp(z::N1 + 1));
    a.pla();
    a.sta(zp(z::N1));
    a.rts();
}

/// `swap_ends`, and the saves the writers end in.
fn saving(a: &mut Asm) {
    // `swap_ends`: CR and LF exchanged through the text before the gap.
    a.here("swap_ends");
    mov16(a, z::P0, z::BUF);
    let swap = a.here_local();
    a.lda(zp(z::P0));
    a.cmp(zp(z::GAP));
    let go_on = a.local();
    a.bne(go_on);
    a.lda(zp(z::P0 + 1));
    a.cmp(zp(z::GAP + 1));
    a.beq(RTS);
    a.bind(go_on);
    a.ldy(imm(0));
    a.lda(zp(z::P0).ind_y());
    a.cmp(imm(CR));
    let not_cr = a.local();
    let store = a.local();
    let next = a.local();
    a.bne(not_cr);
    a.lda(imm(LF));
    a.bne(store);
    a.bind(not_cr);
    a.cmp(imm(LF));
    a.bne(next);
    a.lda(imm(CR));
    a.bind(store);
    a.sta(zp(z::P0).ind_y());
    a.bind(next);
    inc16(a, z::P0);
    a.jmp(swap);

    // `save_range`: the bytes from P1 to P2 saved, with the file's own
    // load and execution addresses; `block_addresses` puts those in the
    // block.
    a.here("save_range");
    a.jsr("block_addresses");
    for (block, from) in [(BLOCK_START, z::P1), (BLOCK_END, z::P2)] {
        a.lda(zp(from));
        a.sta(abs(block));
        a.lda(zp(from + 1));
        a.sta(abs(block + 1));
        a.lda(imm(HERE));
        a.sta(abs(block + 2));
        a.sta(abs(block + 3));
    }
    a.lda(imm(SAVE));
    a.jmp("osfile");

    a.here("block_addresses");
    a.ldx(imm(7));
    let copy = a.here_local();
    a.lda(abs(page::LOAD).x());
    a.sta(abs(BLOCK_LOAD).x());
    a.dex();
    a.bpl(copy);
    a.rts();
}

/// `name_syntax`: the highlighting the file's kind and name ask for: a
/// program's; a boot file's statements, its name's last part `!BOOT` in
/// any case; or none.
fn naming(a: &mut Asm) {
    a.here("name_syntax");
    a.lda(zp(z::FILE));
    a.and(imm(file::BASIC));
    let not_program = a.local();
    a.beq(not_program);
    a.lda(imm(syntax::PROGRAM));
    a.sta(zp(z::SYN));
    a.rts();
    a.bind(not_program);
    a.ldx(imm(0));
    a.ldy(imm(0));
    let scan = a.here_local();
    let scanned = a.local();
    a.lda(abs(page::NAME).x());
    a.cmp(imm(CR));
    a.beq(scanned);
    a.inx();
    a.cmp(imm(b'.'));
    a.bne(scan);
    a.txa();
    a.tay();
    a.jmp(scan);
    a.bind(scanned);
    a.ldx(imm(0));
    let compare = a.here_local();
    let off = a.local();
    a.lda(abs(page::NAME).y());
    a.cmp(imm(b'a'));
    let upper = a.local();
    a.bcc(upper);
    a.cmp(imm(b'z' + 1));
    a.bcs(upper);
    a.and(imm(0xDF));
    a.bind(upper);
    a.cmp(abs("boot_name").x());
    a.bne(off);
    a.iny();
    a.inx();
    a.cpx(imm(crate::rom::asm::byte(BOOT.len())));
    a.bne(compare);
    a.lda(abs(page::NAME).y());
    a.cmp(imm(CR));
    a.bne(off);
    a.lda(imm(syntax::STATEMENTS));
    a.sta(zp(z::SYN));
    a.rts();
    a.bind(off);
    a.lda(imm(syntax::OFF));
    a.sta(zp(z::SYN));
    a.rts();
    a.later(|a| {
        a.here("boot_name");
        a.bytes(BOOT);
    });

    // `close_file`: the file the editor has open closed, where one is.
    a.here("close_file");
    a.ldy(abs(page::HANDLE));
    a.beq(RTS);
    a.lda(imm(0));
    a.sta(abs(page::HANDLE));
    a.jmp(abs(entry::OSFIND));
}
