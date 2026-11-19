//! The command line: a line typed after `:`, its range of lines, its
//! command word, `!`, and its argument; and the commands.

use super::entry::ESCAPE;
use super::normal::DELETE;
use super::ops::{cmp16, inc16, mov16, set16, sub16};
use super::ws::{file, option, page, syntax, zp as z};
use crate::rom::asm::{abs, byte, hi, imm, lo, zp, Asm, RTS};
use crate::rom::os::{entry, osbyte, CR};

/// What the range typed holds, in [`z::RANGE`].
mod range {
    /// One line, in `LINE_B`.
    pub const ONE: u8 = 1;
    /// Two, from `LINE_A` to `LINE_B`.
    pub const TWO: u8 = 2;
}

/// The command words, each with its routine.
const COMMANDS: &[(&str, &str)] = &[
    ("w", "ex_write"),
    ("wq", "ex_write_quit"),
    ("x", "ex_exit"),
    ("q", "ex_quit"),
    ("e", "ex_edit"),
    ("r", "ex_read"),
    ("s", "ex_subst"),
    ("d", "ex_delete"),
    ("set", "ex_set"),
    ("syntax", "ex_syntax"),
    ("syn", "ex_syntax"),
    ("mode", "ex_mode"),
    ("help", "ex_help"),
    ("h", "ex_help"),
];

/// The options `:set` takes, and the words `:syntax` takes.
const OPTIONS: &[(&str, &str)] = &[
    ("nu", "set_number"),
    ("number", "set_number"),
    ("nonu", "set_nonumber"),
    ("nonumber", "set_nonumber"),
    ("ft=basic", "set_basic"),
    ("ft=text", "set_text"),
];
const SYNTAXES: &[(&str, &str)] = &[("on", "syntax_on"), ("off", "syntax_off")];

pub(super) fn emit(a: &mut Asm) {
    line_input(a);
    parsing(a);
    addresses(a);
    words(a);
    files(a);
    names(a);
    lines(a);
    settings(a);
    modes(a);
    os_command(a);
    tables(a);
}

/// `read_line`: a line typed on the status line after the prompt in A,
/// into the input page ending in 0; carry set where ESCAPE, or DELETE on an
/// empty line, gave it up. The widest mode's status line bounds it within
/// the page.
fn line_input(a: &mut Asm) {
    const _: () = {
        let mut widest = 0;
        let mut mode = 0;
        while mode < super::screen::MODES.len() {
            if super::screen::MODES[mode].0 > widest {
                widest = super::screen::MODES[mode].0;
            }
            mode += 1;
        }
        assert!(widest - 2 < page::INPUT_LEN);
    };
    a.here("read_line");
    a.pha();
    a.lda(zp(z::ROWS));
    a.sta(zp(z::ROW));
    a.jsr("tab_row");
    a.pla();
    a.jsr(entry::OSWRCH);
    a.lda(imm(1));
    a.sta(zp(z::WX));
    a.jsr("pad");
    for b in [31u8, 1] {
        a.lda(imm(b));
        a.jsr(entry::OSWRCH);
    }
    a.lda(zp(z::ROWS));
    a.jsr(entry::OSWRCH);
    a.lda(zp(z::WIDTH));
    a.sec();
    a.sbc(imm(2));
    a.sta(zp(z::T4));
    a.lda(imm(0));
    a.sta(zp(z::EX_AT));
    let key = a.here_local();
    let cancel = a.local();
    a.jsr("read_key");
    a.cmp(imm(ESCAPE));
    a.beq(cancel);
    a.cmp(imm(CR));
    let done = a.local();
    a.beq(done);
    a.cmp(imm(DELETE));
    let rub = a.local();
    a.beq(rub);
    a.cmp(imm(b' '));
    a.bcc(key);
    a.cmp(imm(DELETE));
    a.bcs(key);
    a.ldx(zp(z::EX_AT));
    a.cpx(zp(z::T4));
    a.bcs(key);
    a.sta(abs(page::INPUT).x());
    a.inc(zp(z::EX_AT));
    a.jsr(entry::OSWRCH);
    a.jmp(key);
    a.bind(rub);
    a.ldx(zp(z::EX_AT));
    a.beq(cancel);
    a.dec(zp(z::EX_AT));
    a.jsr(entry::OSWRCH);
    a.jmp(key);
    a.bind(done);
    a.ldx(zp(z::EX_AT));
    a.lda(imm(0));
    a.sta(abs(page::INPUT).x());
    a.clc();
    a.rts();
    a.bind(cancel);
    a.sec();
    a.rts();
}

/// `ex_line`: a command line read and done.
fn parsing(a: &mut Asm) {
    a.here("ex_line");
    a.lda(imm(b':'));
    a.jsr("read_line");
    a.bcs(RTS);
    a.lda(imm(0));
    a.sta(zp(z::EX_AT));
    a.sta(zp(z::BANG));
    a.sta(zp(z::RANGE));
    a.jsr("ex_spaces");
    // The range: `%`, or one line or two.
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    a.cmp(imm(b'%'));
    let not_all = a.local();
    a.bne(not_all);
    a.inc(zp(z::EX_AT));
    set16(a, z::LINE_A, 1);
    a.jsr("count_lines");
    mov16(a, z::LINE_B, z::N1);
    a.lda(imm(range::TWO));
    a.sta(zp(z::RANGE));
    let ranged = a.local();
    a.jmp(ranged);
    a.bind(not_all);
    a.jsr("ex_address");
    a.bcs(ranged);
    mov16(a, z::LINE_A, z::N1);
    mov16(a, z::LINE_B, z::N1);
    a.lda(imm(range::ONE));
    a.sta(zp(z::RANGE));
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    a.cmp(imm(b','));
    a.bne(ranged);
    a.inc(zp(z::EX_AT));
    a.jsr("ex_address");
    let bad_range = a.local();
    a.bcs(bad_range);
    mov16(a, z::LINE_B, z::N1);
    a.lda(imm(range::TWO));
    a.sta(zp(z::RANGE));
    a.bind(ranged);
    a.jsr("ex_spaces");
    // `!` alone: an OS command.
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    a.cmp(imm(b'!'));
    let worded = a.local();
    a.bne(worded);
    a.inc(zp(z::EX_AT));
    a.jmp("ex_os");
    a.bind(worded);
    // No word: the line named to go to.
    a.cmp(imm(0));
    let has_word = a.local();
    a.bne(has_word);
    a.lda(zp(z::RANGE));
    a.beq(RTS);
    mov16(a, z::COUNT, z::LINE_B);
    a.jsr("goto_line");
    a.jmp("land");
    a.bind(has_word);
    a.lda(imm(lo("ex_words")));
    a.sta(zp(z::P2));
    a.lda(imm(hi("ex_words")));
    a.sta(zp(z::P2 + 1));
    a.jsr("word_find");
    let unknown = a.local();
    a.bcs(unknown);
    // After the word: `!`, then spaces, then the argument.
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    a.cmp(imm(b'!'));
    let no_bang = a.local();
    a.bne(no_bang);
    a.inc(zp(z::BANG));
    a.inc(zp(z::EX_AT));
    a.bind(no_bang);
    a.jsr("ex_spaces");
    a.jmp(abs(z::P3).ind());
    a.bind(unknown);
    a.call_with_text("say", &["Not an editor command"]);
    a.rts();
    a.bind(bad_range);
    a.here("ex_bad_range");
    a.call_with_text("say", &["Bad range"]);
    a.rts();
}

/// Spaces passed, and the lines a range names.
fn addresses(a: &mut Asm) {
    a.here("ex_spaces");
    a.ldx(zp(z::EX_AT));
    let space = a.here_local();
    a.lda(abs(page::INPUT).x());
    a.cmp(imm(b' '));
    let past = a.local();
    a.bne(past);
    a.inx();
    a.bne(space);
    a.bind(past);
    a.stx(zp(z::EX_AT));
    a.rts();

    // `ex_address`: a line named at EX_AT — `.`, `$` or a number — in N1;
    // carry set where none is.
    a.here("ex_address");
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    a.cmp(imm(b'.'));
    let not_here = a.local();
    a.bne(not_here);
    a.inc(zp(z::EX_AT));
    mov16(a, z::N1, z::LINE);
    a.clc();
    a.rts();
    a.bind(not_here);
    a.cmp(imm(b'$'));
    let not_last = a.local();
    a.bne(not_last);
    a.inc(zp(z::EX_AT));
    a.jsr("count_lines");
    a.clc();
    a.rts();
    a.bind(not_last);
    a.jsr("is_digit");
    let digits = a.local();
    a.bcs(digits);
    a.sec();
    a.rts();
    // A number, a digit at a time, through the count's own adding.
    a.bind(digits);
    a.lda(imm(0));
    a.sta(zp(z::COUNT));
    a.sta(zp(z::COUNT + 1));
    let digit = a.here_local();
    let parsed = a.local();
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    a.jsr("is_digit");
    a.bcc(parsed);
    a.sta(zp(z::KEY));
    a.jsr("add_digit");
    a.inc(zp(z::EX_AT));
    a.jmp(digit);
    a.bind(parsed);
    a.jsr("n1_count");
    a.lda(imm(0));
    a.sta(zp(z::COUNT));
    a.sta(zp(z::COUNT + 1));
    a.clc();
    a.rts();
}

/// `word_find`: the word at `EX_AT` found in the table at P2 — each entry a
/// word ending in 0 and its routine — its routine in P3 and `EX_AT` past it;
/// carry set where the table has no such word.
fn words(a: &mut Asm) {
    a.here("word_find");
    a.ldx(zp(z::EX_AT));
    a.stx(zp(z::T0));
    let letters = a.here_local();
    let ended = a.local();
    a.lda(abs(page::INPUT).x());
    a.cmp(imm(b'='));
    let letter = a.local();
    a.beq(letter);
    a.cmp(imm(b'a'));
    a.bcc(ended);
    a.cmp(imm(b'z' + 1));
    a.bcs(ended);
    a.bind(letter);
    a.inx();
    a.bne(letters);
    a.bind(ended);
    a.stx(zp(z::T1));
    let entry = a.here_local();
    let none = a.local();
    let skip = a.local();
    let found = a.local();
    a.ldy(imm(0));
    a.lda(zp(z::P2).ind_y());
    a.beq(none);
    a.ldx(zp(z::T0));
    let compare = a.here_local();
    let name_end = a.local();
    a.lda(zp(z::P2).ind_y());
    a.beq(name_end);
    a.cmp(abs(page::INPUT).x());
    a.bne(skip);
    a.inx();
    a.iny();
    a.bne(compare);
    a.bind(name_end);
    a.cpx(zp(z::T1));
    a.beq(found);
    a.bind(skip);
    let at_zero = a.local();
    a.lda(zp(z::P2).ind_y());
    a.beq(at_zero);
    a.iny();
    a.bne(skip);
    a.bind(at_zero);
    a.iny();
    a.iny();
    a.iny();
    a.tya();
    a.clc();
    a.adc(zp(z::P2));
    a.sta(zp(z::P2));
    a.bcc(entry);
    a.inc(zp(z::P2 + 1));
    a.jmp(entry);
    a.bind(found);
    a.iny();
    a.lda(zp(z::P2).ind_y());
    a.sta(zp(z::P3));
    a.iny();
    a.lda(zp(z::P2).ind_y());
    a.sta(zp(z::P3 + 1));
    a.ldx(zp(z::T1));
    a.stx(zp(z::EX_AT));
    a.clc();
    a.rts();
    a.bind(none);
    a.sec();
    a.rts();
}

/// The file commands: `:w`, `:wq`, `:x`, `:q` and `:e`, a name from the
/// argument or the file's own.
fn files(a: &mut Asm) {
    const _: () = assert!(file::MODIFIED == 0x80);
    // `unsaved`: back to the main loop where the text changed since it was
    // written and no `!` says to lose that.
    a.here("unsaved");
    a.bit(zp(z::FILE));
    a.bpl(RTS);
    a.lda(zp(z::BANG));
    a.bne(RTS);
    a.call_with_text("say", &["Not written (add !)"]);
    a.jmp("main");

    a.here("no_name");
    a.call_with_text("say", &["No file name"]);
    a.jmp("main");

    a.here("ex_write");
    a.jsr("target_name");
    a.bcs("no_name");
    a.jmp("write_file");

    a.here("ex_write_quit");
    a.jsr("target_name");
    a.bcs("no_name");
    a.jsr("write_file");
    a.jmp("leave");

    // `ZZ` as `:x`, `ZQ` as `:q!`.
    a.here("cmd_z");
    a.jsr("read_key");
    a.cmp(imm(b'Q'));
    a.beq("leave_far");
    a.cmp(imm(b'Z'));
    crate::rom::far(a, Asm::beq, "fail");
    a.lda(imm(0));
    a.sta(zp(z::EX_AT));
    a.sta(abs(page::INPUT));
    a.fall_into("ex_exit");

    a.here("ex_exit");
    a.bit(zp(z::FILE));
    a.bpl("leave_far");
    a.jsr("target_name");
    a.bcs("no_name");
    a.jsr("write_file");
    a.here("leave_far");
    a.jmp("leave");

    a.here("ex_quit");
    a.jsr("unsaved");
    a.jmp("leave");

    // `:e`: the file named, or the file's own again.
    a.here("ex_edit");
    a.jsr("unsaved");
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    let own = a.local();
    a.beq(own);
    a.jsr("arg_to_name");
    let read = a.local();
    a.jmp(read);
    a.bind(own);
    a.lda(abs(page::NAME));
    a.cmp(imm(CR));
    a.beq("no_name");
    a.bind(read);
    a.jmp("open_file");
}

/// The names the file commands write to.
fn names(a: &mut Asm) {
    // `target_name`: the argument's name, which becomes the file's where
    // it has none, or the file's own, in OSFILE's block; carry set where
    // there is neither.
    a.here("target_name");
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    let own = a.local();
    a.beq(own);
    a.jsr("arg_end");
    a.lda(abs(page::NAME));
    a.cmp(imm(CR));
    let pointed = a.local();
    a.bne(pointed);
    a.jsr("arg_to_name");
    a.bind(pointed);
    a.lda(zp(z::EX_AT));
    a.clc();
    a.adc(imm(lo(page::INPUT)));
    a.sta(abs(page::BLOCK));
    a.lda(imm(hi(page::INPUT)));
    a.adc(imm(0));
    a.sta(abs(page::BLOCK + 1));
    a.clc();
    a.rts();
    a.bind(own);
    a.lda(abs(page::NAME));
    a.cmp(imm(CR));
    let none = a.local();
    a.beq(none);
    a.lda(imm(lo(page::NAME)));
    a.sta(abs(page::BLOCK));
    a.lda(imm(hi(page::NAME)));
    a.sta(abs(page::BLOCK + 1));
    a.clc();
    a.rts();
    a.bind(none);
    a.sec();
    a.rts();

    // `arg_end`: the argument ended in CR at its first space.
    a.here("arg_end");
    a.ldx(zp(z::EX_AT));
    let scan = a.here_local();
    let end = a.local();
    a.lda(abs(page::INPUT).x());
    a.beq(end);
    a.cmp(imm(b' '));
    a.beq(end);
    a.inx();
    a.bne(scan);
    a.bind(end);
    a.lda(imm(CR));
    a.sta(abs(page::INPUT).x());
    a.rts();

    // `arg_to_name`: the argument, to its end, made the file's name.
    a.here("arg_to_name");
    a.ldx(zp(z::EX_AT));
    a.ldy(imm(0));
    let copy = a.here_local();
    let copied = a.local();
    a.lda(abs(page::INPUT).x());
    a.beq(copied);
    a.cmp(imm(b' '));
    a.beq(copied);
    a.cmp(imm(CR));
    a.beq(copied);
    a.sta(abs(page::NAME).y());
    a.inx();
    a.iny();
    a.cpy(imm(page::NAME_LEN));
    a.bcc(copy);
    a.bind(copied);
    a.lda(imm(CR));
    a.sta(abs(page::NAME).y());
    a.rts();
}

/// `:d`: the range's lines, or the cursor's, deleted into the register.
fn lines(a: &mut Asm) {
    a.here("ex_delete");
    a.lda(zp(z::RANGE));
    let ranged = a.local();
    a.bne(ranged);
    mov16(a, z::LINE_A, z::LINE);
    mov16(a, z::LINE_B, z::LINE);
    a.bind(ranged);
    cmp16(a, z::LINE_B, z::LINE_A);
    crate::rom::far(a, Asm::bcs, "ex_bad_range");
    mov16(a, z::COUNT, z::LINE_A);
    a.jsr("goto_line");
    a.jsr("rstart_n0");
    mov16(a, z::COUNT, z::LINE_B);
    a.jsr("goto_line");
    a.jsr("line_end");
    a.jsr("n0_vs_len");
    let at_end = a.local();
    a.bcs(at_end);
    inc16(a, z::N0);
    a.bind(at_end);
    a.jsr("rend_n0");
    a.jsr("range_len");
    a.jsr("n0_rstart");
    a.lda(imm(1));
    a.jsr("cut");
    a.jsr("n0_rstart");
    a.jmp("land");
}

/// `:set`, its options, and `:syntax`.
fn settings(a: &mut Asm) {
    a.here("ex_set");
    a.lda(imm(lo("set_words")));
    a.sta(zp(z::P2));
    a.lda(imm(hi("set_words")));
    a.sta(zp(z::P2 + 1));
    a.jsr("word_find");
    let unknown = a.local();
    a.bcs(unknown);
    a.jmp(abs(z::P3).ind());
    a.bind(unknown);
    a.call_with_text("say", &["Unknown option"]);
    a.rts();

    a.here("set_number");
    a.lda(zp(z::OPTIONS));
    a.ora(imm(option::NUMBER));
    let options_set = a.local();
    a.bne(options_set);
    a.here("set_nonumber");
    a.lda(zp(z::OPTIONS));
    a.and(imm(!option::NUMBER));
    a.bind(options_set);
    a.sta(zp(z::OPTIONS));
    a.jmp("cmd_redraw");

    a.here("set_basic");
    a.lda(zp(z::FILE));
    a.ora(imm(file::BASIC));
    let kind_set = a.local();
    a.bne(kind_set);
    a.here("set_text");
    a.lda(zp(z::FILE));
    a.and(imm(!file::BASIC));
    a.bind(kind_set);
    a.sta(zp(z::FILE));
    a.lda(zp(z::SYN));
    crate::rom::far(a, Asm::bne, "cmd_redraw");
    a.jmp("syntax_on");

    a.here("ex_syntax");
    a.lda(imm(lo("syntax_words")));
    a.sta(zp(z::P2));
    a.lda(imm(hi("syntax_words")));
    a.sta(zp(z::P2 + 1));
    a.jsr("word_find");
    a.bcs(unknown);
    a.jmp(abs(z::P3).ind());

    // `syntax_on`: the text highlighted as its kind is.
    a.here("syntax_on");
    a.lda(zp(z::FILE));
    a.and(imm(file::BASIC));
    let text = a.local();
    a.beq(text);
    a.lda(imm(syntax::PROGRAM));
    let chosen = a.local();
    a.bne(chosen);
    a.bind(text);
    a.lda(imm(syntax::STATEMENTS));
    a.bind(chosen);
    a.sta(zp(z::SYN));
    a.jmp("cmd_redraw");

    a.here("syntax_off");
    a.lda(imm(syntax::OFF));
    a.sta(zp(z::SYN));
    a.jmp("cmd_redraw");
}

/// `:mode N`: mode N taken as `change_mode` takes it; one there is not,
/// refused.
fn modes(a: &mut Asm) {
    a.here("ex_mode");
    a.ldx(zp(z::EX_AT));
    a.lda(abs(page::INPUT).x());
    a.sec();
    a.sbc(imm(b'0'));
    a.cmp(imm(byte(super::screen::MODES.len())));
    let bad = a.local();
    a.bcs(bad);
    a.pha();
    a.lda(abs(page::INPUT + 1).x());
    let alone = a.local();
    a.beq(alone);
    a.cmp(imm(b' '));
    a.beq(alone);
    a.pla();
    a.bind(bad);
    a.call_with_text("say", &["Bad mode"]);
    a.rts();
    a.bind(alone);
    a.pla();
    a.fall_into("change_mode");
    changing_mode(a);
    shifting(a);
}

/// `change_mode`: the mode in A taken, a bank taken on where the new screen
/// leaves the text too little room. The gap starts below both modes'
/// HIMEMs, and what lies past it in main memory moves with HIMEM, down
/// before the screen takes the memory or up once it has given it back. What
/// lies in sideways RAM stays where it is, its addresses moving with HIMEM.
fn changing_mode(a: &mut Asm) {
    a.here("change_mode");
    a.sta(zp(z::T0));
    a.lda(imm(osbyte::HIMEM_MODE));
    a.ldx(zp(z::T0));
    a.jsr(entry::OSBYTE);
    a.sty(zp(z::T1));
    a.lda(imm(0));
    a.sta(zp(z::N1));
    a.lda(zp(z::MAIN_TOP));
    a.sec();
    a.sbc(zp(z::T1));
    let takes = a.local();
    a.bcs(takes);
    a.lda(imm(0));
    a.bind(takes);
    a.sta(zp(z::N1 + 1));
    let room = a.here_local();
    sub16(a, z::P0, z::GEND, z::GAP);
    cmp16(a, z::P0, z::N1);
    let fits = a.local();
    a.bcs(fits);
    a.jsr("grow");
    a.bcc(room);
    let too_big = a.local();
    a.jmp(too_big);
    a.bind(fits);
    // The text before the gap below the lower HIMEM.
    a.lda(zp(z::T1));
    a.cmp(zp(z::MAIN_TOP));
    let lower = a.local();
    a.bcc(lower);
    a.lda(zp(z::MAIN_TOP));
    a.bind(lower);
    a.sta(zp(z::T3));
    a.lda(zp(z::GAP));
    a.cmp(imm(1));
    a.lda(zp(z::GAP + 1));
    a.sbc(imm(0));
    a.cmp(zp(z::T3));
    let below = a.local();
    a.bcc(below);
    a.lda(imm(0));
    a.sec();
    a.sbc(zp(z::BUF));
    a.sta(zp(z::N0));
    a.lda(zp(z::T3));
    a.sbc(zp(z::BUF + 1));
    a.sta(zp(z::N0 + 1));
    a.jsr("move_gap");
    a.bind(below);
    // Down while the old HIMEM holds, before the new screen; up once the
    // new one does.
    a.lda(zp(z::T1));
    a.sec();
    a.sbc(zp(z::MAIN_TOP));
    a.sta(zp(z::T4));
    crate::rom::far(a, Asm::bne, "new_mode");
    let up = a.local();
    a.bcs(up);
    a.jsr("past_gap_main");
    let unmoved = a.local();
    a.bcs(unmoved);
    a.jsr("memmove");
    a.bind(unmoved);
    a.jsr("new_mode");
    let shifted = a.local();
    a.jmp(shifted);
    a.bind(up);
    a.jsr("new_mode");
    a.jsr("past_gap_main");
    a.lda(zp(z::T1));
    a.sta(zp(z::MAIN_TOP));
    a.bcs(shifted);
    a.jsr("memmove");
    a.bind(shifted);
    a.lda(zp(z::T4));
    a.jsr("past_gap_by");
    a.lda(zp(z::T1));
    a.sta(zp(z::MAIN_TOP));
    a.rts();
    a.bind(too_big);
    a.call_with_text("say", &["Not enough memory"]);
    a.rts();
}

/// `past_gap_main`: P1, P2 and N1 the move of what lies past the gap in main
/// memory by the pages from the old HIMEM to the new; carry set where
/// nothing does. `new_mode`: the mode in T0 set.
fn shifting(a: &mut Asm) {
    a.here("past_gap_main");
    a.lda(zp(z::GEND + 1));
    a.cmp(zp(z::MAIN_TOP));
    a.bcs(RTS);
    a.jsr("p1_gend");
    mov16(a, z::P2, z::GEND);
    a.lda(zp(z::P2 + 1));
    a.clc();
    a.adc(zp(z::T1));
    a.sec();
    a.sbc(zp(z::MAIN_TOP));
    a.sta(zp(z::P2 + 1));
    a.lda(imm(0));
    a.sec();
    a.sbc(zp(z::GEND));
    a.sta(zp(z::N1));
    a.lda(zp(z::MAIN_TOP));
    a.sbc(zp(z::GEND + 1));
    a.sta(zp(z::N1 + 1));
    a.clc();
    a.rts();

    a.here("new_mode");
    a.lda(zp(z::T0));
    a.sta(zp(z::MODE));
    a.jmp("set_mode");
}

/// `:!command`: an OS command run with the keys given back, its output
/// seen until a key is pressed, then the screen redrawn.
fn os_command(a: &mut Asm) {
    a.here("ex_os");
    a.ldx(zp(z::EX_AT));
    let scan = a.here_local();
    let end = a.local();
    a.lda(abs(page::INPUT).x());
    a.beq(end);
    a.inx();
    a.bne(scan);
    a.bind(end);
    a.lda(imm(CR));
    a.sta(abs(page::INPUT).x());
    a.jsr("give_keys");
    for b in [31u8, 0] {
        a.lda(imm(b));
        a.jsr(entry::OSWRCH);
    }
    a.lda(zp(z::ROWS));
    a.jsr(entry::OSWRCH);
    a.jsr(entry::OSNEWL);
    a.lda(zp(z::EX_AT));
    a.clc();
    a.adc(imm(lo(page::INPUT)));
    a.tax();
    a.lda(imm(hi(page::INPUT)));
    a.adc(imm(0));
    a.tay();
    // The text is not kept while the command runs, since it may start
    // another language, which takes the memory for its own.
    a.lda(imm(0));
    a.sta(abs(page::MARK));
    a.jsr(entry::OSCLI);
    a.jsr("reclaim");
    a.jsr("keep_text");
    a.call_with_text("print_inline", &["\rPress a key"]);
    a.jsr("os_key");
    a.jsr("take_keys");
    a.jmp("set_mode");
}

fn tables(a: &mut Asm) {
    a.later(|a| {
        for (label, entries) in [
            ("ex_words", COMMANDS),
            ("set_words", OPTIONS),
            ("syntax_words", SYNTAXES),
        ] {
            a.here(label);
            for &(word, routine) in entries {
                a.text(word);
                a.byte(0);
                a.word(routine);
            }
            a.byte(0);
        }
    });
}
