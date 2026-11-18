//! `:help`: the commands on one page, then each screen mode's size, its
//! colours and the room it leaves the text in this machine as it stands.
//! Paged, so a screen too small for it stops until SHIFT is pressed.

use super::screen::{Colours, MODES};
use super::ws::{show, zp as z};
use crate::rom::asm::{abs, byte, imm, zp, Asm};
use crate::rom::os::{entry, osbyte};

/// VDU codes: paged scrolling on and off, and the screen cleared.
const VDU_PAGED: u8 = 14;
const VDU_UNPAGED: u8 = 15;
const VDU_CLEAR: u8 = 12;

/// The commands, a few lines to a text `print_inline` prints.
const PAGES: [&str; 3] = [
    "VIM 1.00 for the BBC Micro\r\
     Move  h j k l w b e 0 ^ $ + - RETURN\r\
     \x20     G gg H M L ^F ^B ^D ^U  nG\r\
     Find  /text ?text n N\r\
     Edit  i a I A o O, ESCAPE to stop\r",
    "\x20     x X r ~ J d c y D C Y p P u .\r\
     \x20     dd dw cw yy d$ 3dd 2x\r\
     File  :w [f] :q[!] :wq :x ZZ ZQ\r\
     \x20     :e[!] f  :r f  ^G\r\
     Line  :n :d :n,md :%d :s/a/b/[g] :%s\r",
    "Set   :set nu|nonu|ft=basic|ft=text\r\
     \x20     :syntax on|off  :mode n\r\
     \x20     :dis f  :!cmd  ^L  :help\r\
     \rHighlighting needs 4 colours or more\r\
     Mode Size  Colours Free\r",
];

/// Each [`Colours`]' count, by its value.
const COUNTS: [u8; 4] = [2, 4, 16, 8];
const _: () = {
    assert!(COUNTS[Colours::Two as usize] == 2);
    assert!(COUNTS[Colours::Four as usize] == 4);
    assert!(COUNTS[Colours::Sixteen as usize] == 16);
    assert!(COUNTS[Colours::Teletext as usize] == 8);
};

pub(super) fn emit(a: &mut Asm) {
    a.here("ex_help");
    for code in [VDU_PAGED, VDU_CLEAR] {
        a.lda(imm(code));
        a.jsr(entry::OSWRCH);
    }
    for page in PAGES {
        a.call_with_text("print_inline", &[page]);
    }
    a.ldx(imm(0));
    let mode = a.here_local();
    a.stx(zp(z::T4));
    a.lda(imm(b' '));
    a.jsr(entry::OSWRCH);
    a.txa();
    a.ora(imm(b'0'));
    a.jsr(entry::OSWRCH);
    a.call_with_text("print_inline", &["   "]);
    a.ldx(zp(z::T4));
    a.lda(abs("mode_cols").x());
    a.jsr("help_byte");
    a.lda(imm(b'x'));
    a.jsr(entry::OSWRCH);
    a.ldx(zp(z::T4));
    a.lda(abs("mode_rows").x());
    a.jsr("help_byte");
    a.call_with_text("print_inline", &["  "]);
    a.ldx(zp(z::T4));
    a.ldy(abs("mode_colours").x());
    a.lda(abs("colour_counts").y());
    a.cmp(imm(10));
    let wide = a.local();
    a.bcs(wide);
    a.lda(imm(b' '));
    a.jsr(entry::OSWRCH);
    a.ldy(abs("mode_colours").x());
    a.lda(abs("colour_counts").y());
    a.bind(wide);
    a.jsr("help_byte");
    a.call_with_text("print_inline", &["      "]);
    a.lda(imm(osbyte::HIMEM_MODE));
    a.ldx(zp(z::T4));
    a.jsr(entry::OSBYTE);
    a.txa();
    a.sec();
    a.sbc(zp(z::BUF));
    a.sta(zp(z::N1));
    a.tya();
    a.sbc(zp(z::BUF + 1));
    a.sta(zp(z::N1 + 1));
    a.jsr("decimal");
    a.jsr("print_decimal");
    a.jsr(entry::OSNEWL);
    a.ldx(zp(z::T4));
    a.inx();
    a.cpx(imm(byte(MODES.len())));
    a.bne(mode);
    a.call_with_text("print_inline", &["Press a key"]);
    a.jsr("os_key");
    a.lda(imm(VDU_UNPAGED));
    a.jsr(entry::OSWRCH);
    a.lda(imm(show::ALL));
    a.jmp("owe");

    // `help_byte`: A in decimal.
    a.here("help_byte");
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.sta(zp(z::N1 + 1));
    a.jsr("decimal");
    a.jmp("print_decimal");

    a.later(|a| {
        a.here("colour_counts");
        a.bytes(&COUNTS);
    });
}
