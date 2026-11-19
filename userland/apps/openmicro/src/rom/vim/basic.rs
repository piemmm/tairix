//! BASIC programs: an image listed into the text on load, each line its
//! number and its text as stored, and the text tokenised back on save by
//! BASIC II's rules, as BASIC tokenises a line typed after its number.
//!
//! The listing spells each token, but keeps as stored what the tokeniser
//! kept as typed — strings, the rest after `REM` and `DATA`, star
//! commands — so a program lists and tokenises back to itself.

use tairix_openbasic::keyword::{Flags, PSEUDO_LHS};
use tairix_openbasic::{lineref, program};

use super::buffer::LINE_BUF;
use super::ops::{cmp16, inc16, mov16, set16, sub16};
use super::syntax::state;
use super::vmem::BANK_PAGES;
use super::ws::{page, zp as z};
use crate::rom::asm::{abs, byte, imm, sym, zp, Acc, Asm, Label, RTS};
use crate::rom::far;
use crate::rom::os::CR;

/// The listing's state, in [`z::BASIC_STATE`].
mod listing {
    /// A statement starts here.
    pub const START: u8 = 0x80;
    /// The rest of the line is as stored.
    pub const RAW: u8 = 0x40;
    /// Inside a string.
    pub const QUOTE: u8 = 0x20;
}

/// The highest line number BASIC II takes.
const MOST_LINE: u16 = 32_767;
/// Where `kw_groups` keeps each token's keyword group while a program is
/// listed: the line buffer, which nothing else uses then.
const TOKEN_GROUPS: u16 = LINE_BUF;
/// An image's end, after a line's CR.
const END: u8 = program::END;
/// A line's header: its CR, its number and its length.
const HEADER: u8 = byte(program::HEADER);
/// The execution address BASIC's SAVE gives a program.
const SAVED_EXEC: u16 = crate::rom::basic::SAVED_EXEC;
/// The address high bytes of the machine's own memory.
const HERE: u8 = 0xFF;

pub(super) fn emit(a: &mut Asm) {
    listing(a);
    list_output(a);
    spelling(a);
    grouping(a);
    saving(a);
    refusals(a);
    tokenising(a);
    copying(a);
    numbers(a);
}

/// `basic_list`: the image at `BASIC_IN`, which `is_program` found whole,
/// listed from `BASIC_OUT`; refused where the listing would overtake what
/// is still to be read, or a line pass 255 characters.
fn listing(a: &mut Asm) {
    a.here("basic_list");
    a.jsr("kw_groups");
    let line = a.here_local();
    a.ldy(imm(1));
    a.jsr("rdin");
    a.cmp(imm(END));
    crate::rom::far(a, Asm::bne, "decoded");
    a.sta(zp(z::N1 + 1));
    a.iny();
    a.jsr("rdin");
    a.sta(zp(z::N1));
    a.iny();
    a.jsr("rdin");
    a.sta(zp(z::BASIC_LEN));
    a.lda(imm(0));
    a.sta(zp(z::BASIC_COL));
    a.jsr("list_number");
    a.lda(imm(HEADER));
    a.sta(zp(z::BASIC_AT));
    a.lda(imm(listing::START));
    a.sta(zp(z::BASIC_STATE));
    let body = a.here_local();
    let line_done = a.local();
    let more = a.local();
    a.ldy(zp(z::BASIC_AT));
    a.cpy(zp(z::BASIC_LEN));
    a.bcc(more);
    a.jmp(line_done);
    a.bind(more);
    a.jsr("rdin");
    a.inc(zp(z::BASIC_AT));
    // As stored, the rest of the line or a string's inside.
    let copy = a.local();
    let not_raw = a.local();
    a.bit(zp(z::BASIC_STATE));
    a.bvc(not_raw);
    a.bind(copy);
    a.jsr("list_out");
    a.jmp(body);
    a.bind(not_raw);
    a.tax();
    a.lda(zp(z::BASIC_STATE));
    a.and(imm(listing::QUOTE));
    let not_in_string = a.local();
    a.beq(not_in_string);
    a.txa();
    a.cmp(imm(b'"'));
    a.bne(copy);
    a.lda(zp(z::BASIC_STATE));
    a.and(imm(!(listing::QUOTE | listing::START)));
    a.sta(zp(z::BASIC_STATE));
    a.txa();
    a.bne(copy);
    a.bind(not_in_string);
    a.txa();
    a.cmp(imm(b'"'));
    let not_quote = a.local();
    a.bne(not_quote);
    a.lda(zp(z::BASIC_STATE));
    a.ora(imm(listing::QUOTE));
    a.sta(zp(z::BASIC_STATE));
    a.txa();
    a.bne(copy);
    a.bind(not_quote);
    let not_reference = listed_reference(a, body);
    let plain = listed_token(a, body, not_reference);
    listed_character(a, body, plain);
    listed_line_end(a, line, line_done);
}

/// A line number, its three bytes decoded; the label for what is not one.
fn listed_reference(a: &mut Asm, body: Label) -> Label {
    a.cmp(imm(lineref::MARKER));
    let not_reference = a.local();
    a.bne(not_reference);
    a.ldy(zp(z::BASIC_AT));
    a.jsr("rdin");
    a.sta(zp(z::T1));
    a.asl(Acc);
    a.asl(Acc);
    a.and(imm(lineref::TOP_BITS));
    a.sta(zp(z::N1));
    a.iny();
    a.jsr("rdin");
    a.eor(zp(z::N1));
    a.sta(zp(z::N1));
    a.lda(zp(z::T1));
    for _ in 0..4 {
        a.asl(Acc);
    }
    a.and(imm(lineref::TOP_BITS));
    a.sta(zp(z::N1 + 1));
    a.iny();
    a.jsr("rdin");
    a.eor(zp(z::N1 + 1));
    a.sta(zp(z::N1 + 1));
    a.lda(zp(z::BASIC_AT));
    a.clc();
    a.adc(imm(byte(lineref::ENCODED)));
    a.sta(zp(z::BASIC_AT));
    a.jsr("list_number");
    a.jmp(body);
    not_reference
}

/// A token, spelt, its flags taken as the tokeniser took them; the label
/// for a plain character.
fn listed_token(a: &mut Asm, body: Label, not_reference: Label) -> Label {
    a.bind(not_reference);
    a.cmp(imm(0x80));
    let plain = a.local();
    a.bcc(plain);
    a.jsr("kw_name");
    for (flag, clears, sets) in [
        (Flags::RAW, 0, listing::RAW),
        (Flags::MIDDLE, listing::START, 0),
        (Flags::START, 0, listing::START),
    ] {
        let unflagged = a.local();
        a.lda(zp(z::T3));
        a.and(imm(flag));
        a.beq(unflagged);
        a.lda(zp(z::BASIC_STATE));
        a.and(imm(!clears));
        a.ora(imm(sets));
        a.sta(zp(z::BASIC_STATE));
        a.bind(unflagged);
    }
    a.jmp(body);
    plain
}

/// Any other character: a colon starts a statement, a star there makes the
/// rest a command, a space changes nothing, the rest end the start.
fn listed_character(a: &mut Asm, body: Label, plain: Label) {
    a.bind(plain);
    let copy_here = a.local();
    a.cmp(imm(b':'));
    let not_colon = a.local();
    a.bne(not_colon);
    a.ldx(imm(listing::START));
    a.stx(zp(z::BASIC_STATE));
    a.bne(copy_here);
    a.bind(not_colon);
    a.cmp(imm(b'*'));
    let not_star = a.local();
    a.bne(not_star);
    a.bit(zp(z::BASIC_STATE));
    a.bpl(not_star);
    a.ldx(imm(listing::RAW));
    a.stx(zp(z::BASIC_STATE));
    a.bne(copy_here);
    a.bind(not_star);
    a.cmp(imm(b' '));
    a.beq(copy_here);
    a.tax();
    a.lda(zp(z::BASIC_STATE));
    a.and(imm(!listing::START));
    a.sta(zp(z::BASIC_STATE));
    a.txa();
    a.bind(copy_here);
    a.jsr("list_out");
    a.jmp(body);
}

/// A line's end: its CR, and the next line.
fn listed_line_end(a: &mut Asm, line: Label, line_done: Label) {
    a.bind(line_done);
    a.lda(imm(CR));
    a.jsr("list_out");
    a.lda(zp(z::BASIC_IN));
    a.clc();
    a.adc(zp(z::BASIC_LEN));
    a.sta(zp(z::BASIC_IN));
    let carried = a.local();
    a.bcc(carried);
    a.inc(zp(z::BASIC_IN + 1));
    a.bind(carried);
    a.jmp(line);
}

/// `list_number` and `list_out`: the listing's numbers and characters.
fn list_output(a: &mut Asm) {
    // `list_number`: N1 listed in decimal.
    a.here("list_number");
    a.jsr("decimal");
    a.ldx(imm(0));
    let digit = a.here_local();
    a.cpx(zp(z::T0));
    a.beq(RTS);
    a.lda(abs(page::DIGITS).x());
    a.jsr("list_out");
    a.inx();
    a.bne(digit);
    a.rts();

    // `list_out`: A listed at BASIC_OUT, short of what is still to be
    // read; a line's characters counted to its most. X and Y kept.
    a.here("list_out");
    a.sty(zp(z::T4));
    a.pha();
    let check = a.here_local();
    a.lda(zp(z::BASIC_OUT + 1));
    a.cmp(zp(z::BASIC_IN + 1));
    let fits = a.local();
    let over = a.local();
    a.bcc(fits);
    a.bne(over);
    a.lda(zp(z::BASIC_OUT));
    a.cmp(zp(z::BASIC_IN));
    a.bcs(over);
    a.bind(fits);
    a.pla();
    a.jsr("wrout");
    a.ldy(zp(z::T4));
    a.cmp(imm(CR));
    a.beq(RTS);
    a.inc(zp(z::BASIC_COL));
    a.bne(RTS);
    a.jsr("say_too_long");
    a.jmp("decode_failed");
    // Caught up: a bank taken on and what is still to be read moved up into
    // it, out of the way, the keyword being listed kept.
    a.bind(over);
    a.txa();
    a.pha();
    for v in [z::N1, z::N1 + 1, z::P3, z::P3 + 1] {
        a.lda(zp(v));
        a.pha();
    }
    a.jsr("grow");
    let refused = a.local();
    a.bcs(refused);
    mov16(a, z::P1, z::BASIC_IN);
    mov16(a, z::P2, z::BASIC_IN);
    a.lda(zp(z::P2 + 1));
    a.clc();
    a.adc(imm(BANK_PAGES));
    a.sta(zp(z::P2 + 1));
    sub16(a, z::N1, z::GEND, z::P2);
    a.jsr("memmove");
    a.lda(zp(z::BASIC_IN + 1));
    a.clc();
    a.adc(imm(BANK_PAGES));
    a.sta(zp(z::BASIC_IN + 1));
    for v in [z::P3 + 1, z::P3, z::N1 + 1, z::N1] {
        a.pla();
        a.sta(zp(v));
    }
    a.pla();
    a.tax();
    a.jmp(check);
    a.bind(refused);
    for _ in 0..6 {
        a.pla();
    }
    a.jsr("say_too_big");
    a.jmp("decode_failed");
}

/// `kw_name`: the token in A listed as its keyword's spelling, an assigning
/// pseudo-variable's as the variable's, found in the one group of keywords
/// `kw_groups` names; its flags in T3. A byte no keyword has is listed as it
/// is.
fn spelling(a: &mut Asm) {
    a.here("kw_name");
    a.sta(zp(z::T2));
    a.and(imm(0x7F));
    a.tax();
    a.lda(abs(TOKEN_GROUPS).x());
    let none = a.local();
    a.bmi(none);
    a.tax();
    a.asl(Acc);
    a.tay();
    a.lda(abs("kw_index").y());
    a.sta(zp(z::P3));
    a.lda(abs(sym("kw_index") + 1).y());
    a.sta(zp(z::P3 + 1));
    let entry = a.here_local();
    let found = a.local();
    let skip = a.local();
    a.ldy(imm(0));
    a.lda(zp(z::P3).ind_y());
    a.beq(none);
    a.jsr("kw_token");
    a.cmp(zp(z::T2));
    a.beq(found);
    a.iny();
    a.lda(zp(z::P3).ind_y());
    a.dey();
    a.and(imm(Flags::PSEUDO));
    a.beq(skip);
    a.lda(zp(z::P3).ind_y());
    a.ora(imm(PSEUDO_LHS));
    a.cmp(zp(z::T2));
    a.beq(found);
    a.bind(skip);
    a.jsr("kw_next");
    a.jmp(entry);
    a.bind(none);
    a.lda(imm(0));
    a.sta(zp(z::T3));
    a.lda(zp(z::T2));
    a.jmp("list_out");
    // Y at the token found.
    a.bind(found);
    a.iny();
    a.lda(zp(z::P3).ind_y());
    a.sta(zp(z::T3));
    a.txa();
    a.clc();
    a.adc(imm(b'A'));
    a.jsr("list_out");
    a.ldy(imm(0));
    let letter = a.here_local();
    a.lda(zp(z::P3).ind_y());
    a.bmi(RTS);
    a.jsr("list_out");
    a.iny();
    a.bne(letter);
    a.rts();
}

/// `kw_groups`: each token's keyword group, by its first letter, kept where
/// [`TOKEN_GROUPS`] lies, &FF for a byte no keyword has, so `kw_name` scans
/// one group, not every keyword, for each token it lists.
fn grouping(a: &mut Asm) {
    a.here("kw_groups");
    a.ldx(imm(0x7F));
    a.lda(imm(0xFF));
    let fill = a.here_local();
    a.sta(abs(TOKEN_GROUPS).x());
    a.dex();
    a.bpl(fill);
    a.ldx(imm(0));
    let group = a.here_local();
    a.txa();
    a.asl(Acc);
    a.tay();
    a.lda(abs("kw_index").y());
    a.sta(zp(z::P3));
    a.lda(abs(sym("kw_index") + 1).y());
    a.sta(zp(z::P3 + 1));
    let entry = a.here_local();
    let next_group = a.local();
    let advance = a.local();
    a.ldy(imm(0));
    a.lda(zp(z::P3).ind_y());
    a.beq(next_group);
    a.jsr("kw_token");
    a.sta(zp(z::T2));
    a.sty(zp(z::T1));
    a.and(imm(0x7F));
    a.tay();
    a.txa();
    a.sta(abs(TOKEN_GROUPS).y());
    a.ldy(zp(z::T1));
    a.iny();
    a.lda(zp(z::P3).ind_y());
    a.and(imm(Flags::PSEUDO));
    a.beq(advance);
    a.lda(zp(z::T2));
    a.ora(imm(PSEUDO_LHS));
    a.and(imm(0x7F));
    a.tay();
    a.txa();
    a.sta(abs(TOKEN_GROUPS).y());
    a.bind(advance);
    a.ldy(zp(z::T1));
    a.jsr("kw_next");
    a.jmp(entry);
    a.bind(next_group);
    a.inx();
    a.cpx(imm(26));
    a.bne(group);
    a.rts();
}

/// `basic_emit`: the text tokenised into a program image put out through
/// `out_byte`, its lines as BASIC takes typed ones: a number to 32767,
/// above the line before, a blank line passed over. A line it refuses is
/// named, and nothing more is put out.
fn saving(a: &mut Asm) {
    a.here("basic_emit");
    a.lda(imm(0));
    a.sta(zp(z::BASIC_STATE));
    a.sta(zp(z::N0));
    a.sta(zp(z::N0 + 1));
    set16(a, z::RSTART, 1);
    let line = a.here_local();
    let more = a.local();
    let finish = a.local();
    let blank = a.local();
    let no_number = a.local();
    let too_big = a.local();
    let out_of_order = a.local();
    let too_long = a.local();
    a.jsr("n0_vs_len");
    a.bcc(more);
    a.jmp(finish);
    a.bind(more);
    a.jsr("fetch_line");
    a.ldy(imm(0));
    let space = a.here_local();
    let spaced = a.local();
    a.lda(abs(LINE_BUF).y());
    a.cmp(imm(b' '));
    a.bne(spaced);
    a.iny();
    a.bne(space);
    a.bind(spaced);
    a.cmp(imm(CR));
    far(a, Asm::bne, blank);
    a.jsr("is_digit");
    far(a, Asm::bcs, no_number);
    a.sty(zp(z::BASIC_AT));
    a.jsr("read_number");
    far(a, Asm::bcc, too_big);
    a.lda(zp(z::N1 + 1));
    a.cmp(imm(byte(usize::from(MOST_LINE >> 8)) + 1));
    far(a, Asm::bcc, too_big);
    a.bit(zp(z::BASIC_STATE));
    let first = a.local();
    a.bpl(first);
    cmp16(a, z::LAST_NUMBER, z::N1);
    far(a, Asm::bcc, out_of_order);
    a.bind(first);
    a.lda(imm(0x80));
    a.sta(zp(z::BASIC_STATE));
    mov16(a, z::LAST_NUMBER, z::N1);
    // The header, then the text tokenised, then the length the header
    // carries; REND, the line's start, follows the line where a part of the
    // program is written out under it.
    mov16(a, z::REND, z::BASIC_OUT);
    a.lda(imm(CR));
    a.jsr("out_byte");
    a.lda(zp(z::LAST_NUMBER + 1));
    a.jsr("out_byte");
    a.lda(zp(z::LAST_NUMBER));
    a.jsr("out_byte");
    a.lda(imm(0));
    a.jsr("out_byte");
    a.jsr("tokenise_body");
    sub16(a, z::N1, z::BASIC_OUT, z::REND);
    a.lda(zp(z::N1 + 1));
    far(a, Asm::beq, too_long);
    a.ldy(imm(HEADER - 1));
    a.lda(zp(z::N1));
    a.sta(zp(z::REND).ind_y());
    // The next text line.
    a.bind(blank);
    a.lda(zp(z::N0));
    a.sec();
    a.adc(zp(z::LLEN));
    a.sta(zp(z::N0));
    let carried = a.local();
    a.bcc(carried);
    a.inc(zp(z::N0 + 1));
    a.bind(carried);
    inc16(a, z::RSTART);
    a.jmp(line);
    a.bind(finish);
    a.lda(imm(CR));
    a.jsr("out_byte");
    a.lda(imm(END));
    a.jmp("out_byte");
    for (label, reason) in [
        (no_number, " has no number"),
        (too_big, ": number past 32767"),
        (out_of_order, ": out of order"),
        (too_long, " too long"),
    ] {
        a.bind(label);
        a.jsr("refused_line");
        a.call_with_text("msg_text", &[reason]);
        a.jmp("refused_end");
    }
}

/// `refused_line` and `refused_end`: a refused line named; and the
/// addresses BASIC saves a program with.
fn refusals(a: &mut Asm) {
    a.here("refused_line");
    a.jsr("msg_begin");
    a.call_with_text("msg_text", &["Line "]);
    mov16(a, z::N1, z::RSTART);
    a.jmp("msg_number");

    a.here("refused_end");
    a.jsr("msg_end");
    a.jmp("main");

    // `basic_addresses`: a program its file gave no addresses saved as
    // BASIC saves one: loaded at PAGE, entered at BASIC's own start.
    a.here("basic_addresses");
    a.ldx(imm(7));
    a.lda(imm(0));
    let any = a.here_local();
    a.ora(abs(page::LOAD).x());
    a.dex();
    a.bpl(any);
    a.tax();
    a.bne(RTS);
    a.lda(zp(z::BUF));
    a.sta(abs(page::LOAD));
    a.lda(zp(z::BUF + 1));
    a.sta(abs(page::LOAD + 1));
    let [exec_low, exec_high] = SAVED_EXEC.to_le_bytes();
    a.lda(imm(exec_low));
    a.sta(abs(page::EXEC));
    a.lda(imm(exec_high));
    a.sta(abs(page::EXEC + 1));
    a.lda(imm(HERE));
    for at in [
        page::LOAD + 2,
        page::LOAD + 3,
        page::EXEC + 2,
        page::EXEC + 3,
    ] {
        a.sta(abs(at));
    }
    a.rts();
}

/// `tokenise_body`: the line `LINE_BUF` holds, from `BASIC_AT`, tokenised to
/// `BASIC_OUT` as BASIC II tokenises a typed line's text after its number:
/// a statement's start, where a number is a line number.
fn tokenising(a: &mut Asm) {
    a.here("tokenise_body");
    a.lda(imm(state::START | state::NUMBERS));
    a.sta(zp(z::LEX));
    a.here("tok_next");
    a.ldy(zp(z::BASIC_AT));
    a.lda(abs(LINE_BUF).y());
    a.cmp(imm(CR));
    let go = a.local();
    a.bne(go);
    a.rts();
    a.bind(go);
    // A keyword: its token, an assigning one for a pseudo-variable at a
    // statement's start.
    let not_upper = a.local();
    let not_keyword = a.local();
    a.cmp(imm(b'A'));
    a.bcc(not_upper);
    a.cmp(imm(b'Z' + 1));
    a.bcs(not_upper);
    a.jsr("kw_match");
    a.bcc(not_keyword);
    a.stx(zp(z::BASIC_AT));
    a.sta(zp(z::T2));
    a.bit(zp(z::LEX));
    let put = a.local();
    a.bpl(put);
    a.lda(zp(z::T3));
    a.and(imm(Flags::PSEUDO));
    a.beq(put);
    a.lda(zp(z::T2));
    a.ora(imm(PSEUDO_LHS));
    a.sta(zp(z::T2));
    a.bind(put);
    a.lda(zp(z::T2));
    a.jsr("out_byte");
    a.jsr("keyword_flags");
    a.lda(zp(z::T3));
    a.and(imm(Flags::NAME));
    let unnamed = a.local();
    a.beq(unnamed);
    a.jsr("copy_name");
    a.bind(unnamed);
    a.lda(zp(z::T3));
    a.and(imm(Flags::RAW));
    let not_raw = a.local();
    a.beq(not_raw);
    a.jmp("copy_rest");
    a.bind(not_raw);
    a.jmp("tok_next");
    a.bind(not_keyword);
    a.lda(abs(LINE_BUF).y());
    a.bind(not_upper);
    let (not_colon, copy_one) = tokenised_name(a);
    let not_digit = tokenised_literal(a, not_colon);
    // Any other character, as typed, the statement's start passed.
    a.bind(not_digit);
    a.jsr("clear_state");
    a.jmp(copy_one);

    a.here("tok_next_far");
    a.jmp("tok_next");
}

/// A name, as typed; a space, comma or colon copied, a colon starting a
/// statement. The labels for what is none of those and for the copy.
fn tokenised_name(a: &mut Asm) -> (Label, Label) {
    let not_name = a.local();
    a.cmp(imm(b'A'));
    a.bcc(not_name);
    a.jsr("is_name");
    a.bcc(not_name);
    a.jsr("clear_state");
    a.jsr("copy_name");
    a.jmp("tok_next");
    a.bind(not_name);
    let copy_one = a.local();
    let not_colon = a.local();
    a.cmp(imm(b' '));
    a.beq(copy_one);
    a.cmp(imm(b','));
    a.beq(copy_one);
    a.cmp(imm(b':'));
    a.bne(not_colon);
    a.lda(imm(state::START));
    a.sta(zp(z::LEX));
    a.bind(copy_one);
    a.jsr("copy_char");
    a.jmp("tok_next");
    (not_colon, copy_one)
}

/// A string as typed to its closing quote, a hexadecimal number, a star
/// command at a statement's start, or a number, a line number's reference
/// where one stands; the label for anything else.
fn tokenised_literal(a: &mut Asm, not_colon: Label) -> Label {
    a.bind(not_colon);
    a.cmp(imm(b'"'));
    let not_string = a.local();
    a.bne(not_string);
    a.jsr("copy_char");
    let inside = a.here_local();
    let closing = a.local();
    a.ldy(zp(z::BASIC_AT));
    a.lda(abs(LINE_BUF).y());
    a.cmp(imm(CR));
    a.beq("tok_next_far");
    a.cmp(imm(b'"'));
    a.beq(closing);
    a.jsr("copy_char");
    a.jmp(inside);
    a.bind(closing);
    a.jsr("copy_char");
    a.jsr("clear_state");
    a.jmp("tok_next");
    // A hexadecimal number.
    a.bind(not_string);
    a.cmp(imm(b'&'));
    let not_hex = a.local();
    a.bne(not_hex);
    a.jsr("copy_char");
    let hex = a.here_local();
    a.ldy(zp(z::BASIC_AT));
    a.lda(abs(LINE_BUF).y());
    a.jsr("is_hex");
    a.bcc("tok_next_far");
    a.jsr("copy_char");
    a.jmp(hex);
    // A star command at a statement's start: the rest as typed.
    a.bind(not_hex);
    a.cmp(imm(b'*'));
    let not_star = a.local();
    a.bne(not_star);
    a.bit(zp(z::LEX));
    a.bpl(not_star);
    a.jmp("copy_rest");
    // A number: where line numbers stand, its reference, the state kept.
    a.bind(not_star);
    a.cmp(imm(b'.'));
    let number = a.local();
    a.beq(number);
    a.jsr("is_digit");
    let not_digit = a.local();
    a.bcc(not_digit);
    a.bit(zp(z::LEX));
    a.bvc(number);
    a.lda(zp(z::BASIC_AT));
    a.pha();
    a.jsr("read_number");
    let past_reference = a.local();
    a.bcs(past_reference);
    a.pla();
    a.jsr("put_reference");
    a.jmp("tok_next");
    a.bind(past_reference);
    a.pla();
    a.sta(zp(z::BASIC_AT));
    a.bind(number);
    a.jsr("copy_number");
    a.lda(zp(z::LEX));
    a.and(imm(!state::START));
    a.sta(zp(z::LEX));
    a.jmp("tok_next");
    not_digit
}

/// The tokeniser's copies: a character, a name, the rest of a line and a
/// number, each as typed.
fn copying(a: &mut Asm) {
    // `copy_char`: LINE_BUF's character at BASIC_AT put out as typed.
    a.here("copy_char");
    a.ldy(zp(z::BASIC_AT));
    a.lda(abs(LINE_BUF).y());
    a.inc(zp(z::BASIC_AT));
    a.jmp("out_byte");

    // `copy_name`: the name characters from BASIC_AT, as typed.
    a.here("copy_name");
    a.ldy(zp(z::BASIC_AT));
    a.lda(abs(LINE_BUF).y());
    a.jsr("is_name");
    a.bcc(RTS);
    a.jsr("copy_char");
    a.jmp("copy_name");

    // `copy_rest`: the rest of the line, as typed.
    a.here("copy_rest");
    a.ldy(zp(z::BASIC_AT));
    a.lda(abs(LINE_BUF).y());
    a.cmp(imm(CR));
    a.beq(RTS);
    a.jsr("copy_char");
    a.jmp("copy_rest");

    // `copy_number`: a number's digits and points, as typed.
    a.here("copy_number");
    a.jsr("copy_char");
    let digits = a.here_local();
    a.ldy(zp(z::BASIC_AT));
    a.lda(abs(LINE_BUF).y());
    a.cmp(imm(b'.'));
    let point = a.local();
    a.beq(point);
    a.jsr("is_digit");
    a.bcc(RTS);
    a.bind(point);
    a.jsr("copy_char");
    a.jmp(digits);
}

/// `read_number`: the decimal number at `BASIC_AT` in N1, `BASIC_AT` past it;
/// carry set where it passes 65535. `put_reference`: N1 put out as a line
/// number's reference.
fn numbers(a: &mut Asm) {
    a.here("read_number");
    a.lda(imm(0));
    a.sta(zp(z::N1));
    a.sta(zp(z::N1 + 1));
    let digit = a.here_local();
    let read = a.local();
    a.ldy(zp(z::BASIC_AT));
    a.lda(abs(LINE_BUF).y());
    a.jsr("is_digit");
    a.bcc(read);
    a.jsr("times_ten_add");
    a.bcs(RTS);
    a.inc(zp(z::BASIC_AT));
    a.jmp(digit);
    a.bind(read);
    a.clc();
    a.rts();

    // `times_ten_add`: N1 ten times over with the digit A added; carry set
    // where it passes 65535.
    a.here("times_ten_add");
    a.and(imm(0x0F));
    a.pha();
    let over = a.local();
    a.asl(zp(z::N1));
    a.rol(zp(z::N1 + 1));
    a.bcs(over);
    mov16(a, z::N2, z::N1);
    for _ in 0..2 {
        a.asl(zp(z::N1));
        a.rol(zp(z::N1 + 1));
        a.bcs(over);
    }
    a.clc();
    a.lda(zp(z::N1));
    a.adc(zp(z::N2));
    a.sta(zp(z::N1));
    a.lda(zp(z::N1 + 1));
    a.adc(zp(z::N2 + 1));
    a.sta(zp(z::N1 + 1));
    a.bcs(over);
    a.pla();
    a.clc();
    a.adc(zp(z::N1));
    a.sta(zp(z::N1));
    a.lda(zp(z::N1 + 1));
    a.adc(imm(0));
    a.sta(zp(z::N1 + 1));
    a.rts();
    a.bind(over);
    a.pla();
    a.sec();
    a.rts();

    a.here("put_reference");
    a.lda(imm(lineref::MARKER));
    a.jsr("out_byte");
    a.lda(zp(z::N1));
    a.and(imm(lineref::TOP_BITS));
    a.lsr(Acc);
    a.lsr(Acc);
    a.sta(zp(z::T1));
    a.lda(zp(z::N1 + 1));
    a.and(imm(lineref::TOP_BITS));
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.ora(zp(z::T1));
    a.eor(imm(lineref::FIRST_FLIP));
    a.jsr("out_byte");
    for half in [z::N1, z::N1 + 1] {
        a.lda(zp(half));
        a.and(imm(lineref::LOW_BITS));
        a.ora(imm(lineref::PRINTABLE));
        a.jsr("out_byte");
    }
    a.rts();
}
