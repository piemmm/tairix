//! Highlighting: a line [`LINE_BUF`] holds cut into runs of one class, by
//! BASIC II's tokenising rules, and the keyword matching those rules and the
//! tokeniser share, so what is coloured a keyword is what a save tokenises.

use tairix_openbasic::basic2::KEYWORDS;
use tairix_openbasic::keyword::{Flags, Keyword};

use super::buffer::LINE_BUF;
use super::screen::class;
use super::ws::{syntax, zp as z};
use crate::rom::asm::{abs, imm, sym, zp, Acc, Asm, Label, RTS};
use crate::rom::os::CR;

/// The lexer's state, in [`z::LEX`].
pub(super) mod state {
    /// A statement starts here.
    pub const START: u8 = 0x80;
    /// A number here is a line number.
    pub const NUMBERS: u8 = 0x40;
    /// A program's line, its number still to come.
    pub const FIRST: u8 = 0x20;
    /// The rest of the line is as typed, after `REM` or `DATA`.
    pub const RAW: u8 = 0x10;
    /// A name follows, after `FN` or `PROC`.
    pub const NAME: u8 = 0x08;
}

/// What a keyword's flags set in the lexer's state, once its token has
/// cleared the line-number state, in the tokeniser's order: each flag, and
/// the state bits it clears and sets.
const KEYWORD_STATES: [(u8, u8, u8); 5] = [
    (Flags::MIDDLE, state::START, 0),
    (Flags::START, 0, state::START),
    (Flags::LINE_NUMBER, 0, state::NUMBERS),
    (Flags::NAME, 0, state::NAME),
    (Flags::RAW, 0, state::RAW),
];

/// A run's longest: the rest of any line.
const WHOLE: u8 = 0xFF;

pub(super) fn emit(a: &mut Asm) {
    lexer(a);
    lex_run(a);
    runs(a);
    matching(a);
    entry_walks(a);
    classes(a);
    table(a);
}

/// `lex_start`: the lexer at a line's start. `lex_run`: the run from
/// column Y, its class in RCLASS and its length in RLEN; T0 to T3 and P3
/// used.
fn lexer(a: &mut Asm) {
    const _: () = assert!(state::START == 0x80 && state::NUMBERS == 0x40);
    a.here("lex_start");
    a.lda(zp(z::SYN));
    a.cmp(imm(syntax::PROGRAM));
    let statements = a.local();
    a.bne(statements);
    a.lda(imm(state::START | state::FIRST));
    a.sta(zp(z::LEX));
    a.rts();
    a.bind(statements);
    a.lda(imm(state::START));
    a.sta(zp(z::LEX));
    a.rts();
}

/// `lex_run`'s start: a run where nothing is highlighted, the rest of a
/// line as stored, the name after FN or PROC, and a program line's number.
fn lex_run(a: &mut Asm) {
    a.here("lex_run");
    a.sty(zp(z::T0));
    a.lda(zp(z::SYN));
    let lexing = a.local();
    a.bne(lexing);
    a.lda(imm(class::NORMAL));
    a.jmp("run_rest");
    a.bind(lexing);
    a.lda(zp(z::LEX));
    a.and(imm(state::RAW));
    let not_raw = a.local();
    a.beq(not_raw);
    a.lda(imm(class::COMMENT));
    a.jmp("run_rest");

    // The name after FN or PROC.
    a.bind(not_raw);
    a.lda(zp(z::LEX));
    a.and(imm(state::NAME));
    let not_name = a.local();
    a.beq(not_name);
    a.lda(zp(z::LEX));
    a.and(imm(!state::NAME));
    a.sta(zp(z::LEX));
    a.jsr("name_run");
    a.cpx(zp(z::T0));
    a.beq(not_name);
    a.lda(imm(class::NORMAL));
    a.jmp("run_to_x");

    // A program line's number, after any spaces.
    a.bind(not_name);
    a.lda(zp(z::LEX));
    a.and(imm(state::FIRST));
    let statement = a.local();
    a.beq(statement);
    a.ldy(zp(z::T0));
    a.lda(abs(LINE_BUF).y());
    a.cmp(imm(b' '));
    let numbered = a.local();
    a.bne(numbered);
    a.lda(imm(class::NORMAL));
    a.jmp("run_one");
    a.bind(numbered);
    a.lda(zp(z::LEX));
    a.and(imm(!state::FIRST));
    a.sta(zp(z::LEX));
    a.lda(abs(LINE_BUF).y());
    a.jsr("is_digit");
    a.bcc(statement);
    a.jsr("digit_run");
    a.lda(imm(class::NUMBER));
    a.jmp("run_to_x");
    let not_star = lexed_word(a, statement);
    lexed_number(a, not_star);
}

/// A keyword, a name, a string, a hexadecimal number or a star command;
/// the label for none of those.
fn lexed_word(a: &mut Asm, statement: Label) -> Label {
    a.bind(statement);
    a.ldy(zp(z::T0));
    a.lda(abs(LINE_BUF).y());
    a.cmp(imm(b'A'));
    let not_upper = a.local();
    a.bcc(not_upper);
    a.cmp(imm(b'Z' + 1));
    a.bcs(not_upper);
    a.jsr("kw_match");
    let not_keyword = a.local();
    a.bcc(not_keyword);
    a.jsr("keyword_flags");
    a.lda(imm(class::KEYWORD));
    a.jmp("run_to_x");

    // A name.
    a.bind(not_keyword);
    a.lda(abs(LINE_BUF).y());
    a.bind(not_upper);
    a.cmp(imm(b'A'));
    let not_ident = a.local();
    a.bcc(not_ident);
    a.jsr("is_name");
    a.bcc(not_ident);
    a.jsr("name_run");
    a.jsr("clear_state");
    a.lda(imm(class::NORMAL));
    a.jmp("run_to_x");

    // A string, to its closing quote.
    a.bind(not_ident);
    a.cmp(imm(b'"'));
    let not_string = a.local();
    a.bne(not_string);
    a.ldx(zp(z::T0));
    let inside = a.here_local();
    let closed = a.local();
    a.inx();
    a.lda(abs(LINE_BUF).x());
    a.cmp(imm(CR));
    a.beq(closed);
    a.cmp(imm(b'"'));
    a.bne(inside);
    a.inx();
    a.bind(closed);
    a.jsr("clear_state");
    a.lda(imm(class::STRING));
    a.jmp("run_to_x");

    // A hexadecimal number.
    a.bind(not_string);
    a.cmp(imm(b'&'));
    let not_hex = a.local();
    a.bne(not_hex);
    a.ldx(zp(z::T0));
    let hex = a.here_local();
    a.inx();
    a.lda(abs(LINE_BUF).x());
    a.jsr("is_hex");
    a.bcs(hex);
    a.lda(imm(class::NUMBER));
    a.jmp("run_to_x");

    // A star command at a statement's start: the rest of the line.
    a.bind(not_hex);
    a.cmp(imm(b'*'));
    let not_star = a.local();
    a.bne(not_star);
    a.bit(zp(z::LEX));
    a.bpl(not_star);
    a.lda(imm(class::COMMAND));
    a.jmp("run_rest");
    not_star
}

/// A number, its digits and points, a line number keeping the state; a
/// colon starting a statement; or any other character.
fn lexed_number(a: &mut Asm, not_star: Label) {
    a.bind(not_star);
    a.ldy(zp(z::T0));
    a.lda(abs(LINE_BUF).y());
    a.cmp(imm(b'.'));
    let number = a.local();
    a.beq(number);
    a.jsr("is_digit");
    let not_number = a.local();
    a.bcc(not_number);
    a.bind(number);
    a.ldx(zp(z::T0));
    let digits = a.here_local();
    a.inx();
    a.lda(abs(LINE_BUF).x());
    a.cmp(imm(b'.'));
    a.beq(digits);
    a.jsr("is_digit");
    a.bcs(digits);
    a.bit(zp(z::LEX));
    let kept = a.local();
    a.bvs(kept);
    a.lda(zp(z::LEX));
    a.and(imm(!state::START));
    a.sta(zp(z::LEX));
    a.bind(kept);
    a.lda(imm(class::NUMBER));
    a.jmp("run_to_x");

    // A colon starts a statement; a space or comma changes nothing; any
    // other character ends the statement's start.
    a.bind(not_number);
    a.cmp(imm(b':'));
    let not_colon = a.local();
    a.bne(not_colon);
    a.lda(imm(state::START));
    a.sta(zp(z::LEX));
    let plain = a.local();
    a.bne(plain);
    a.bind(not_colon);
    a.cmp(imm(b' '));
    a.beq(plain);
    a.cmp(imm(b','));
    a.beq(plain);
    a.jsr("clear_state");
    a.bind(plain);
    a.lda(imm(class::NORMAL));
    a.jmp("run_one");
}

/// The runs `lex_run` ends with: of class A, to column X, of one column, or
/// the rest of the line; and the walks they measure.
fn runs(a: &mut Asm) {
    a.here("run_to_x");
    a.sta(zp(z::RCLASS));
    a.txa();
    a.sec();
    a.sbc(zp(z::T0));
    a.sta(zp(z::RLEN));
    a.rts();

    a.here("run_one");
    a.sta(zp(z::RCLASS));
    a.lda(imm(1));
    a.sta(zp(z::RLEN));
    a.rts();

    a.here("run_rest");
    a.sta(zp(z::RCLASS));
    a.lda(imm(WHOLE));
    a.sta(zp(z::RLEN));
    a.rts();

    // `keyword_flags`: the state a keyword with the flags in T3 leaves, as
    // the tokeniser leaves it. X and Y kept.
    a.here("keyword_flags");
    a.lda(zp(z::LEX));
    a.and(imm(!state::NUMBERS));
    for (flag, clears, sets) in KEYWORD_STATES {
        let unflagged = a.local();
        a.pha();
        a.lda(zp(z::T3));
        a.and(imm(flag));
        a.beq(unflagged);
        a.pla();
        a.and(imm(!clears));
        a.ora(imm(sets));
        a.pha();
        a.bind(unflagged);
        a.pla();
    }
    a.sta(zp(z::LEX));
    a.rts();

    // `clear_state`: no statement start and no line numbers from here.
    a.here("clear_state");
    a.lda(zp(z::LEX));
    a.and(imm(!(state::START | state::NUMBERS)));
    a.sta(zp(z::LEX));
    a.rts();

    // `name_run`, `digit_run`: X past the name characters, or the digits,
    // from column T0.
    a.here("name_run");
    a.ldx(zp(z::T0));
    let name = a.here_local();
    let named = a.local();
    a.lda(abs(LINE_BUF).x());
    a.jsr("is_name");
    a.bcc(named);
    a.inx();
    a.bne(name);
    a.bind(named);
    a.rts();

    a.here("digit_run");
    a.ldx(zp(z::T0));
    let digit = a.here_local();
    let counted = a.local();
    a.lda(abs(LINE_BUF).x());
    a.jsr("is_digit");
    a.bcc(counted);
    a.inx();
    a.bne(digit);
    a.bind(counted);
    a.rts();
}

/// `kw_match`: the keyword the text at column Y starts, by BASIC II's
/// rules: the first in its letter's run spelt whole, a conditional one only
/// before no name character, or the first a `.` abbreviates. Carry set with
/// its token in A, its flags in T3 and the column past it in X; Y kept.
fn matching(a: &mut Asm) {
    a.here("kw_match");
    a.sty(zp(z::T0));
    // Every keyword's second letter is a capital, so a name whose second
    // character is neither that nor a point is no keyword.
    let none = a.local();
    let candidate = a.local();
    a.lda(abs(LINE_BUF + 1).y());
    a.cmp(imm(b'.'));
    a.beq(candidate);
    a.sbc(imm(b'A'));
    a.cmp(imm(26));
    a.bcs(none);
    a.bind(candidate);
    a.lda(abs(LINE_BUF).y());
    a.sec();
    a.sbc(imm(b'A'));
    a.asl(Acc);
    a.tax();
    a.lda(abs("kw_index").x());
    a.sta(zp(z::P3));
    a.lda(abs(sym("kw_index") + 1).x());
    a.sta(zp(z::P3 + 1));
    let entry = a.here_local();
    let have = a.local();
    a.ldy(imm(0));
    a.lda(zp(z::P3).ind_y());
    a.bne(have);
    a.ldy(zp(z::T0));
    a.bind(none);
    a.clc();
    a.rts();
    a.bind(have);
    a.ldx(zp(z::T0));
    a.inx();
    let compare = a.here_local();
    let whole = a.local();
    let differ = a.local();
    let matched = a.local();
    let next = a.local();
    a.lda(zp(z::P3).ind_y());
    a.bmi(whole);
    a.cmp(abs(LINE_BUF).x());
    a.bne(differ);
    a.inx();
    a.iny();
    a.bne(compare);
    // Spelt whole: Y at its token.
    a.bind(whole);
    a.jsr("kw_take");
    a.and(imm(Flags::CONDITIONAL));
    a.beq(matched);
    a.lda(abs(LINE_BUF).x());
    a.jsr("is_name");
    a.bcc(matched);
    // Back from the flags `kw_take` reached to the token.
    a.dey();
    a.bcs(next);
    // Abbreviated by a point where its spelling stops.
    a.bind(differ);
    a.lda(abs(LINE_BUF).x());
    a.cmp(imm(b'.'));
    a.bne(next);
    a.inx();
    a.jsr("kw_token");
    a.jsr("kw_take");
    a.bind(matched);
    a.lda(zp(z::T2));
    a.ldy(zp(z::T0));
    a.sec();
    a.rts();
    a.bind(next);
    a.jsr("kw_next");
    a.jmp(entry);
}

/// The walks across an entry at P3 every reader of the table shares.
/// `kw_token`: Y moved on from a place in the entry's letters to its token,
/// which is in A. `kw_take`: the token in A kept in T2, Y moved to the flags
/// after it and those kept in T3 and A. `kw_next`: P3 moved to the next
/// entry from Y, a place in this one no further than its token. X kept by
/// each.
fn entry_walks(a: &mut Asm) {
    a.here("kw_token");
    a.lda(zp(z::P3).ind_y());
    a.bmi(RTS);
    a.iny();
    a.bne("kw_token");
    a.rts();

    a.here("kw_take");
    a.sta(zp(z::T2));
    a.iny();
    a.lda(zp(z::P3).ind_y());
    a.sta(zp(z::T3));
    a.rts();

    a.here("kw_next");
    a.jsr("kw_token");
    a.iny();
    a.iny();
    a.tya();
    a.clc();
    a.adc(zp(z::P3));
    a.sta(zp(z::P3));
    a.bcc(RTS);
    a.inc(zp(z::P3 + 1));
    a.rts();
}

/// `is_name`, `is_digit`, `is_hex`: carry set where A may be part of a
/// name, is a digit, or a hexadecimal digit as BASIC II reads one.
fn classes(a: &mut Asm) {
    let no = a.local();
    let yes = a.local();
    a.here("is_hex");
    a.cmp(imm(b'A'));
    let digit = a.local();
    a.bcc(digit);
    a.cmp(imm(b'F' + 1));
    a.bcc(yes);
    a.bcs(no);
    a.here("is_name");
    a.cmp(imm(b'A'));
    a.bcc(digit);
    a.cmp(imm(b'Z' + 1));
    a.bcc(yes);
    a.cmp(imm(b'_'));
    a.beq(yes);
    a.cmp(imm(b'`'));
    a.beq(yes);
    a.cmp(imm(b'a'));
    a.bcc(no);
    a.cmp(imm(b'z' + 1));
    a.bcc(yes);
    a.bcs(no);
    a.here("is_digit");
    a.bind(digit);
    a.cmp(imm(b'0'));
    a.bcc(no);
    a.cmp(imm(b'9' + 1));
    a.bcc(yes);
    a.bind(no);
    a.clc();
    a.rts();
    a.bind(yes);
    a.sec();
    a.rts();
}

/// The keyword table: an address for each letter's run of entries, each
/// entry the letters after its first, then its token, which ends them, and
/// its flags; a zero ends each run. The runs keep BASIC II's table order.
fn table(a: &mut Asm) {
    a.later(|a| {
        a.here("kw_index");
        for letter in b'A'..=b'Z' {
            a.word(run_label(letter));
        }
        for letter in b'A'..=b'Z' {
            a.here(run_label(letter));
            for Keyword { word, flags } in KEYWORDS {
                let name = word.name.as_bytes();
                if name[0] != letter {
                    continue;
                }
                a.bytes(&name[1..]);
                a.byte(word.token);
                a.byte(flags.bits());
            }
            a.byte(0);
        }
    });
}

// A token ends its entry's letters, so none of them may look like one; and
// `kw_match` passes over a name whose second character is no capital.
const _: () = {
    let mut at = 0;
    while at < KEYWORDS.len() {
        let name = KEYWORDS[at].word.name.as_bytes();
        assert!(name.len() > 1 && name[1].is_ascii_uppercase());
        let mut letter = 1;
        while letter < name.len() {
            assert!(name[letter] < 0x80);
            letter += 1;
        }
        assert!(KEYWORDS[at].word.token >= 0x80);
        at += 1;
    }
};

/// The label of the run of keywords starting with `letter`.
fn run_label(letter: u8) -> &'static str {
    const LABELS: [&str; 26] = [
        "kw_a", "kw_b", "kw_c", "kw_d", "kw_e", "kw_f", "kw_g", "kw_h", "kw_i", "kw_j", "kw_k",
        "kw_l", "kw_m", "kw_n", "kw_o", "kw_p", "kw_q", "kw_r", "kw_s", "kw_t", "kw_u", "kw_v",
        "kw_w", "kw_x", "kw_y", "kw_z",
    ];
    LABELS[usize::from(letter - b'A')]
}
