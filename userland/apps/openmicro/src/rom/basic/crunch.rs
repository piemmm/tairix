//! Tokenising a typed line in place: keywords become their tokens, a line's
//! own number and the numbers after a line-number keyword become encoded
//! references, and strings, REM and DATA tails, star commands and the names
//! after FN and PROC are left as typed.

use super::tokens::{flag, Keyword, KEYWORDS, LINE_REFERENCE, PSEUDO_LHS};
use super::ws::page;
use super::ws::zp as z;
use crate::rom::asm::{hi, imm, lo, zp, Acc, Asm, RTS};

/// Where the next character is read from, and the next byte written to.
pub(super) const READ: u16 = z::SCRATCH + 2;
pub(super) const WRITE: u16 = READ + 1;
/// Whether a statement starts here, and whether numbers are line numbers.
const STATE: u16 = WRITE + 1;
const AT_START: u8 = 0x80;
const LINE_NUMBERS: u8 = 0x40;
/// The keyword being tried, and its flags once one matches, shifted right
/// past CONDITIONAL so each comes off in turn.
pub(super) const KEYWORD: u16 = STATE + 1;
const FLAGS: u16 = KEYWORD + 2;
/// A line number being read, and its value before the digit's.
pub(super) const NUMBER: u16 = FLAGS + 1;
const BEFORE: u16 = NUMBER + 2;
/// The text being read.
pub(super) const FROM: u16 = BEFORE + 2;
/// Where the input goes on after a keyword.
const END: u16 = FROM + 2;

const CR: u8 = 0x0D;
/// An encoded line number's bytes, its token among them.
pub(super) const REFERENCE: u8 = 4;
/// A table entry's flags byte, which only a keyword with flags has, bears
/// this bit, as its token does.
const FLAGGED: u8 = 0x80;
/// The order the flags come off in.
const _: () = assert!(
    flag::CONDITIONAL == 1
        && flag::MIDDLE == 2
        && flag::START == 4
        && flag::LINE_NUMBER == 8
        && flag::NAME == 0x10
        && flag::RAW == 0x20
);
/// The pseudo-variables' tokens, which at a statement's start take their
/// assigning ones: those flagged PSEUDO, which run unbroken.
const PSEUDO_TOKENS: (u8, u8) = pseudo_tokens();

/// # Panics
///
/// At compile time, if the pseudo-variables' tokens are not one run, or
/// a keyword's flags reach the flags byte's mark.
const fn pseudo_tokens() -> (u8, u8) {
    let (mut first, mut last, mut count) = (0xFF_u8, 0_u8, 0_u8);
    let mut i = 0;
    while i < KEYWORDS.len() {
        let Keyword { token, flags, .. } = KEYWORDS[i];
        assert!(flags & FLAGGED == 0);
        if flags & flag::PSEUDO != 0 {
            if token < first {
                first = token;
            }
            if token > last {
                last = token;
            }
            count += 1;
        }
        i += 1;
    }
    assert!(count == last - first + 1);
    (first, last)
}

pub fn emit(a: &mut Asm) {
    crunch(a);
    crunch_items(a);
    copying(a);
    matching(a);
    line_numbers(a);
    encoding(a);
    table(a);
}

/// The table: each keyword's name, then its token and any flags; a zero
/// ends it.
fn table(a: &mut Asm) {
    a.here("keywords");
    for Keyword {
        name, token, flags, ..
    } in KEYWORDS
    {
        a.text(name);
        a.byte(token);
        if flags != 0 {
            a.byte(FLAGGED | flags);
        }
    }
    a.byte(0);
}

/// Tokenise the input buffer from offset Y to its CR into its start, a
/// line number first encoded, ending with the CR; X the length before the
/// CR. The text from XA's start is tokenised as a statement, or as an
/// expression from a statement's middle.
fn crunch(a: &mut Asm) {
    a.here("crunch_statement");
    a.ldy(imm(AT_START));
    let at = a.local();
    a.bne(at);
    a.here("crunch_expression");
    a.ldy(imm(0));
    a.bind(at);
    a.sty(zp(STATE));
    a.ldy(imm(0));
    a.beq("crunch_from");

    a.here("crunch");
    a.ldx(imm(AT_START | LINE_NUMBERS));
    a.stx(zp(STATE));
    a.lda(imm(lo(page::INPUT)));
    a.ldx(imm(hi(page::INPUT)));
    a.here("crunch_from");
    a.sta(zp(FROM));
    a.stx(zp(FROM + 1));
    a.sty(zp(READ));
    a.lda(imm(0));
    a.sta(zp(WRITE));
    a.beq("crunch_next");

    a.here("crunch_end");
    a.ldy(zp(WRITE));
    a.sta(zp(FROM).ind_y());
    a.tya();
    a.tax();
    a.rts();

    // A keyword: at a statement's start a pseudo-variable takes its
    // assigning token. What it leaves: no line numbers unless it takes them;
    // the middle of a statement, or a new one; a name or the rest of the
    // line as typed.
    a.here("crunch_token");
    let write = a.local();
    a.bit(zp(STATE));
    a.bpl(write);
    a.cmp(imm(PSEUDO_TOKENS.0));
    a.bcc(write);
    a.cmp(imm(PSEUDO_TOKENS.1 + 1));
    a.bcs(write);
    a.ora(imm(PSEUDO_LHS));
    a.bind(write);
    a.jsr("write_byte");
    a.lda(zp(STATE));
    a.and(imm(!LINE_NUMBERS));
    let (middle, start, lines, named) = (a.local(), a.local(), a.local(), a.local());
    a.lsr(zp(FLAGS));
    a.bcc(middle);
    a.lda(imm(0));
    a.bind(middle);
    a.lsr(zp(FLAGS));
    a.bcc(start);
    a.lda(imm(AT_START));
    a.bind(start);
    a.lsr(zp(FLAGS));
    a.bcc(lines);
    a.ora(imm(LINE_NUMBERS));
    a.bind(lines);
    a.sta(zp(STATE));
    a.lsr(zp(FLAGS));
    a.bcc(named);
    a.jsr("copy_name");
    a.bind(named);
    a.lsr(zp(FLAGS));
    a.bcc("crunch_next");
    a.fall_into("crunch_raw");

    // The rest of the line as typed.
    a.here("crunch_raw");
    a.jsr("peek");
    let raw = a.here_local();
    a.cmp(imm(CR));
    a.beq("crunch_end");
    a.jsr("copy_peek");
    a.jmp(raw);
}

fn crunch_items(a: &mut Asm) {
    a.here("crunch_next");
    a.jsr("peek");
    let punctuation = a.local();
    a.cmp(imm(b'A'));
    a.bcc(punctuation);
    a.cmp(imm(b'Z' + 1));
    a.bcs("crunch_name");
    a.jsr("keyword_match");
    a.bcs("crunch_token");
    // No keyword starts here, so its letter starts a name.
    a.jsr("peek");

    // A name, copied whole so no keyword is found inside it.
    a.here("crunch_name");
    a.jsr("is_name");
    a.bcc("crunch_other");
    a.lda(imm(0));
    a.sta(zp(STATE));
    a.jsr("copy_name");
    a.bcc("crunch_next");

    a.bind(punctuation);
    a.cmp(imm(CR));
    a.beq("crunch_end");
    a.cmp(imm(b' '));
    a.beq("crunch_keep");
    a.cmp(imm(b','));
    a.beq("crunch_keep");
    a.cmp(imm(b':'));
    a.beq("crunch_colon");
    a.cmp(imm(b'"'));
    a.beq("crunch_string");
    a.cmp(imm(b'&'));
    a.beq("crunch_hex");
    a.cmp(imm(b'*'));
    let not_star = a.local();
    a.bne(not_star);
    a.bit(zp(STATE));
    a.bmi("crunch_raw");
    a.bind(not_star);
    a.jsr("is_digit");
    let not_digit = a.local();
    a.bcc(not_digit);
    a.bit(zp(STATE));
    a.bvc("crunch_number");
    a.jmp("crunch_line_number");
    a.bind(not_digit);
    a.cmp(imm(b'.'));
    a.beq("crunch_number");
    a.fall_into("crunch_other");

    // Anything else ends the statement's start and any line numbers.
    a.here("crunch_other");
    a.ldx(imm(0));
    a.stx(zp(STATE));
    a.here("crunch_keep");
    a.jsr("copy_byte");
    a.jmp("crunch_next");

    a.here("crunch_colon");
    a.ldx(imm(AT_START));
    a.stx(zp(STATE));
    a.bne("crunch_keep");

    // A string, to its closing quote or the line's end.
    a.here("crunch_string");
    a.jsr("copy_peek");
    a.cmp(imm(b'"'));
    a.beq("crunch_other");
    a.cmp(imm(CR));
    a.bne("crunch_string");
    a.jmp("crunch_end");

    // A hex number's digits, among which no keyword is found.
    a.here("crunch_hex");
    a.jsr("copy_peek");
    a.jsr("is_digit");
    a.bcs("crunch_hex");
    a.cmp(imm(b'A'));
    let ended = a.local();
    a.bcc(ended);
    a.cmp(imm(b'F' + 1));
    a.bcc("crunch_hex");
    a.bind(ended);
    a.jmp("crunch_next");

    // A number that is not a line number: its digits and points.
    a.here("crunch_number");
    a.jsr("copy_peek");
    a.cmp(imm(b'.'));
    a.beq("crunch_number");
    a.jsr("is_digit");
    a.bcs("crunch_number");
    a.lda(zp(STATE));
    a.and(imm(!AT_START));
    a.sta(zp(STATE));
    a.jmp("crunch_next");
}

fn copying(a: &mut Asm) {
    // The byte at the read position copied, then as peek.
    a.here("copy_peek");
    a.jsr("copy_byte");
    a.fall_into("peek");

    // A the byte at the read position, Y the position.
    a.here("peek");
    a.ldy(zp(READ));
    a.lda(zp(FROM).ind_y());
    a.rts();

    // The byte read written, both positions moved on.
    a.here("copy_byte");
    a.jsr("peek");
    a.inc(zp(READ));
    a.here("write_byte");
    a.ldy(zp(WRITE));
    a.sta(zp(FROM).ind_y());
    a.inc(zp(WRITE));
    a.rts();

    // A name's letters, digits, underscores and pounds, copied; C clear.
    a.here("copy_name");
    a.jsr("peek");
    let letter = a.here_local();
    a.jsr("is_name");
    a.bcc(RTS);
    a.jsr("copy_peek");
    a.jmp(letter);

    // C set if A is a digit.
    a.here("is_digit");
    a.cmp(imm(b'9' + 1));
    let no = a.local();
    a.bcs(no);
    a.cmp(imm(b'0'));
    a.rts();
    a.bind(no);
    a.clc();
    a.rts();

    // C set if A can be part of a name: a letter, digit, `_` or `£`;
    // letters, the most of them, tested first.
    a.here("is_name");
    a.cmp(imm(b'A'));
    a.bcc("is_digit");
    a.cmp(imm(b'Z' + 1));
    let letter = a.local();
    a.bcc(letter);
    a.cmp(imm(b'_'));
    a.bcc(no);
    a.cmp(imm(b'z' + 1));
    a.bcs(no);
    a.bind(letter);
    a.sec();
    a.rts();
}

/// Matching a keyword at the read position. The keyword pointer is biased
/// by the read position, so one index reads the input and the keyword.
fn matching(a: &mut Asm) {
    // C set if a keyword starts at the read position: A its token, its
    // flags kept, the read position past it; a word ending in `.` is the
    // first keyword it begins. A conditional keyword followed by part of a
    // name is no keyword.
    a.here("keyword_match");
    a.lda(imm(lo("keywords")));
    a.sec();
    a.sbc(zp(READ));
    a.sta(zp(KEYWORD));
    a.lda(imm(hi("keywords")));
    a.sbc(imm(0));
    a.sta(zp(KEYWORD + 1));
    let (try_, compare, whole, differs, skip, accept) = (
        a.here_local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.ldy(zp(READ));
    a.bind(compare);
    a.lda(zp(KEYWORD).ind_y());
    a.bmi(whole);
    a.cmp(zp(FROM).ind_y());
    a.bne(differs);
    a.iny();
    a.bne(compare);
    a.bind(differs);
    a.cpy(zp(READ));
    a.beq(skip);
    a.lda(zp(FROM).ind_y());
    a.cmp(imm(b'.'));
    a.bne(skip);
    // Abbreviated: the input goes on after the point, the token follows the
    // keyword's letters.
    a.iny();
    a.sty(zp(END));
    a.dey();
    let token = a.here_local();
    a.iny();
    a.lda(zp(KEYWORD).ind_y());
    a.bpl(token);
    a.pha();
    a.jsr("keyword_flags");
    a.bpl(accept);
    a.bind(whole);
    a.sty(zp(END));
    a.pha();
    a.jsr("keyword_flags");
    a.bcc(accept);
    a.ldy(zp(END));
    a.lda(zp(FROM).ind_y());
    a.jsr("is_name");
    a.bcc(accept);
    a.pla();
    a.ldy(zp(END));
    // To the next keyword past this one's token, or the end.
    a.bind(skip);
    let past = a.here_local();
    a.lda(zp(KEYWORD).ind_y());
    a.iny();
    a.cmp(imm(0x80));
    a.bcc(past);
    a.jsr("next_keyword");
    a.bne(try_);
    a.clc();
    a.rts();
    a.bind(accept);
    a.lda(zp(END));
    a.sta(zp(READ));
    a.pla();
    a.sec();
    a.rts();

    // FLAGS the flags after the token Y is at, none where there are none;
    // C their CONDITIONAL, N clear.
    a.here("keyword_flags");
    a.iny();
    a.lda(zp(KEYWORD).ind_y());
    let flagged = a.local();
    a.bmi(flagged);
    a.lda(imm(0));
    a.bind(flagged);
    a.lsr(Acc);
    a.sta(zp(FLAGS));
    a.rts();

    // KEYWORD on to the next entry, Y just past this one's token: Y the
    // read position, A the entry's first byte, Z at the table's end.
    a.here("next_keyword");
    a.lda(zp(KEYWORD).ind_y());
    let bare = a.local();
    a.bpl(bare);
    a.iny();
    a.bind(bare);
    a.tya();
    a.sec();
    a.sbc(zp(READ));
    a.ldx(imm(lo(KEYWORD)));
    a.jsr("pointer_add");
    a.ldy(zp(READ));
    a.lda(zp(KEYWORD).ind_y());
    a.rts();
}

/// A line number after a keyword that takes one: its digits read and
/// encoded, or left as typed if they overflow sixteen bits.
fn line_numbers(a: &mut Asm) {
    a.here("crunch_line_number");
    a.lda(imm(0));
    a.sta(zp(NUMBER));
    a.sta(zp(NUMBER + 1));
    a.ldy(zp(READ));
    a.here("crunch_line_digit");
    a.lda(zp(FROM).ind_y());
    a.jsr("is_digit");
    a.bcc("crunch_line_read");
    a.and(imm(0x0F));
    a.jsr("ten_times_add");
    let fits = a.local();
    a.bcc(fits);
    a.jmp("crunch_number");
    a.bind(fits);
    a.iny();
    a.bne("crunch_line_digit");

    // &8D, then the top two bits of each byte, flipped, in the first, and
    // each byte's low six; bit 6 set in all three. A reference longer than
    // its digits moves the rest of the line up to make room.
    // Reached only with C clear: a digit's absence, or no overflow.
    a.here("crunch_line_read");
    a.sty(zp(READ));
    a.lda(zp(WRITE));
    a.adc(imm(REFERENCE));
    a.sec();
    a.sbc(zp(READ));
    let fits = a.local();
    a.bcc(fits);
    a.beq(fits);
    a.sta(zp(BEFORE));
    a.clc();
    a.adc(zp(FROM));
    a.sta(zp(END));
    a.lda(zp(FROM + 1));
    a.adc(imm(0));
    a.sta(zp(END + 1));
    let (find, moving) = (a.here_local(), a.local());
    a.lda(zp(FROM).ind_y());
    a.cmp(imm(CR));
    a.beq(moving);
    a.iny();
    a.bne(find);
    a.bind(moving);
    let moved = a.local();
    let move_byte = a.here_local();
    a.lda(zp(FROM).ind_y());
    a.sta(zp(END).ind_y());
    a.cpy(zp(READ));
    a.beq(moved);
    a.dey();
    a.bne(move_byte);
    a.bind(moved);
    a.lda(zp(READ));
    a.clc();
    a.adc(zp(BEFORE));
    a.sta(zp(READ));
    a.bind(fits);
    a.lda(imm(LINE_REFERENCE));
    a.jsr("write_byte");
    a.jsr("encode_number");
    a.jmp("crunch_next");
}

fn encoding(a: &mut Asm) {
    // NUMBER's three encoded bytes written.
    a.here("encode_number");
    a.lda(zp(NUMBER + 1));
    a.and(imm(0xC0));
    a.lsr(Acc);
    a.lsr(Acc);
    a.sta(zp(BEFORE));
    a.lda(zp(NUMBER));
    a.and(imm(0xC0));
    a.ora(zp(BEFORE));
    a.lsr(Acc);
    a.lsr(Acc);
    a.eor(imm(0x54));
    a.jsr("write_byte");
    a.lda(zp(NUMBER));
    a.and(imm(0x3F));
    a.ora(imm(0x40));
    a.jsr("write_byte");
    a.lda(zp(NUMBER + 1));
    a.and(imm(0x3F));
    a.ora(imm(0x40));
    a.jmp("write_byte");

    // NUMBER = NUMBER × 10 + A, C set if that overflows sixteen bits; Y is
    // kept.
    a.here("ten_times_add");
    a.pha();
    a.lda(zp(NUMBER));
    a.sta(zp(BEFORE));
    a.lda(zp(NUMBER + 1));
    a.sta(zp(BEFORE + 1));
    let over = a.local();
    for step in 0..4 {
        if step == 2 {
            a.lda(zp(NUMBER));
            a.adc(zp(BEFORE));
            a.sta(zp(NUMBER));
            a.lda(zp(NUMBER + 1));
            a.adc(zp(BEFORE + 1));
            a.sta(zp(NUMBER + 1));
        } else {
            a.asl(zp(NUMBER));
            a.rol(zp(NUMBER + 1));
        }
        a.bcs(over);
    }
    a.pla();
    a.adc(zp(NUMBER));
    a.sta(zp(NUMBER));
    a.bcc(RTS);
    a.inc(zp(NUMBER + 1));
    let overflow = a.local();
    a.beq(overflow);
    a.clc();
    a.rts();
    a.bind(over);
    a.pla();
    a.bind(overflow);
    a.sec();
    a.rts();
}
