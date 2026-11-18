//! The status line's messages, built a piece at a time: `msg_begin`, then
//! characters, the file's name, numbers and texts, then `msg_end`; `say`
//! makes the text after its call the whole message.

use super::ws::{page, show, zp as z};
use crate::rom::asm::{abs, hi, imm, lo, zp, Asm, RTS};
use crate::rom::os::CR;

pub(super) fn emit(a: &mut Asm) {
    a.here("msg_begin");
    a.lda(imm(0));
    a.sta(zp(z::MSG_AT));
    a.rts();

    // `msg_put`: A added, where the message has room. A and Y kept.
    a.here("msg_put");
    a.ldx(zp(z::MSG_AT));
    a.cpx(imm(page::MESSAGE_LEN));
    a.bcs(RTS);
    a.sta(abs(page::MESSAGE).x());
    a.inc(zp(z::MSG_AT));
    a.rts();

    // `msg_end`: the message ended and shown. Y kept.
    a.here("msg_end");
    a.ldx(zp(z::MSG_AT));
    a.lda(imm(0));
    a.sta(abs(page::MESSAGE).x());
    a.lda(imm(show::MESSAGE));
    a.jmp("owe");

    // `msg_name`: the file's name, in quotes; `msg_name_at` the name at
    // P0, ending in CR.
    a.here("msg_name");
    a.lda(imm(lo(page::NAME)));
    a.sta(zp(z::P0));
    a.lda(imm(hi(page::NAME)));
    a.sta(zp(z::P0 + 1));
    a.here("msg_name_at");
    a.lda(imm(b'"'));
    a.jsr("msg_put");
    a.ldy(imm(0));
    let next = a.here_local();
    let named = a.local();
    a.lda(zp(z::P0).ind_y());
    a.cmp(imm(CR));
    a.beq(named);
    a.jsr("msg_put");
    a.iny();
    a.cpy(imm(page::NAME_LEN));
    a.bcc(next);
    a.bind(named);
    a.lda(imm(b'"'));
    a.jmp("msg_put");

    // `msg_number`: N1 in decimal.
    a.here("msg_number");
    a.jsr("decimal");
    a.ldy(imm(0));
    let digit = a.here_local();
    a.cpy(zp(z::T0));
    a.beq(RTS);
    a.lda(abs(page::DIGITS).y());
    a.jsr("msg_put");
    a.iny();
    a.bne(digit);
    a.rts();

    // `say`: the text after the call the message; `msg_text`: added to it.
    a.here("say");
    a.jsr("msg_begin");
    a.lda(imm(1));
    let carried = a.local();
    a.bne(carried);
    a.here("msg_text");
    a.lda(imm(0));
    a.bind(carried);
    a.sta(zp(z::MSG_ENDS));
    a.pla();
    a.sta(zp(z::P0));
    a.pla();
    a.sta(zp(z::P0 + 1));
    a.ldy(imm(0));
    let take = a.here_local();
    let taken = a.local();
    a.iny();
    a.lda(zp(z::P0).ind_y());
    a.beq(taken);
    a.jsr("msg_put");
    a.jmp(take);
    a.bind(taken);
    a.lda(zp(z::MSG_ENDS));
    let past = a.local();
    a.beq(past);
    a.jsr("msg_end");
    a.bind(past);
    a.jmp("return_past");

    // The messages more than one refusal gives.
    for (name, message) in [
        ("say_not_file", "Not a file"),
        ("say_too_big", "File too big"),
        ("say_no_room", "No room"),
        ("say_no_pattern", "No previous pattern"),
        ("say_too_long", "Line too long"),
    ] {
        a.here(name);
        a.call_with_text("say", &[message]);
        a.rts();
    }
}
