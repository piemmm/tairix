//! BASIC's errors: their numbers and messages as the User's Guide lists
//! them, an entry point that raises each, and the report through IERROR.

use alloc::vec::Vec;

use super::tokens::{token, KEYWORDS};
use super::ws::{vector, zp};
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Acc, Asm};
use crate::rom::c64::kernal::entry as kernal;
use crate::rom::c64::kernal::files::error as kernal_error;
use crate::rom::c64::kernal::ws::zp::CONTROL_MESSAGES;

/// The messages from error 1; the first nine are the KERNAL's.
const MESSAGES: [&str; 30] = [
    "TOO MANY FILES",
    "FILE OPEN",
    "FILE NOT OPEN",
    "FILE NOT FOUND",
    "DEVICE NOT PRESENT",
    "NOT INPUT FILE",
    "NOT OUTPUT FILE",
    "MISSING FILE NAME",
    "ILLEGAL DEVICE NUMBER",
    "NEXT WITHOUT FOR",
    "SYNTAX",
    "RETURN WITHOUT GOSUB",
    "OUT OF DATA",
    "ILLEGAL QUANTITY",
    "OVERFLOW",
    "OUT OF MEMORY",
    "UNDEF'D STATEMENT",
    "BAD SUBSCRIPT",
    "REDIM'D ARRAY",
    "DIVISION BY ZERO",
    "ILLEGAL DIRECT",
    "TYPE MISMATCH",
    "STRING TOO LONG",
    "FILE DATA",
    "FORMULA TOO COMPLEX",
    "CAN'T CONTINUE",
    "UNDEF'D FUNCTION",
    "VERIFY",
    "LOAD",
    "BREAK",
];

/// Error numbers.
pub mod number {
    pub const NEXT_WITHOUT_FOR: u8 = 10;
    pub const SYNTAX: u8 = 11;
    pub const RETURN_WITHOUT_GOSUB: u8 = 12;
    pub const OUT_OF_DATA: u8 = 13;
    pub const ILLEGAL_QUANTITY: u8 = 14;
    pub const OVERFLOW: u8 = 15;
    pub const OUT_OF_MEMORY: u8 = 16;
    pub const UNDEFD_STATEMENT: u8 = 17;
    pub const BAD_SUBSCRIPT: u8 = 18;
    pub const REDIMD_ARRAY: u8 = 19;
    pub const DIVISION_BY_ZERO: u8 = 20;
    pub const ILLEGAL_DIRECT: u8 = 21;
    pub const TYPE_MISMATCH: u8 = 22;
    pub const STRING_TOO_LONG: u8 = 23;
    pub const FILE_DATA: u8 = 24;
    pub const FORMULA_TOO_COMPLEX: u8 = 25;
    pub const CANT_CONTINUE: u8 = 26;
    pub const UNDEFD_FUNCTION: u8 = 27;
    pub const VERIFY: u8 = 28;
    pub const LOAD: u8 = 29;
    pub const BREAK: u8 = 30;
    /// What IERROR is entered with for no error: READY.
    pub const READY: u8 = 0x80;
}

/// The errors raised from more than one place, each through a routine of
/// its own; the rest are raised where they occur.
const RAISERS: [(&str, u8); 7] = [
    ("illegal_quantity_error", number::ILLEGAL_QUANTITY),
    ("syntax_error", number::SYNTAX),
    ("overflow_error", number::OVERFLOW),
    ("out_of_memory_error", number::OUT_OF_MEMORY),
    ("bad_subscript_error", number::BAD_SUBSCRIPT),
    ("type_mismatch_error", number::TYPE_MISMATCH),
    ("string_too_long_error", number::STRING_TOO_LONG),
];

/// Error `number` raised here.
pub fn raise(a: &mut Asm, number: u8) {
    a.ldx(imm(number));
    a.jmp("error");
}

/// CURLIN's high byte in direct mode.
pub const DIRECT_MODE: u8 = 0xFF;

/// The text after an error's message, and before a line number.
const ERROR_SUFFIX: &str = "  ERROR";
const IN: &str = " IN ";
/// READY, which [`NEW_LINE`] follows; a message's first text is that too.
const READY: &str = "\r\nREADY.";
const NEW_LINE: &str = "\r\n";

/// The first code a message spells a keyword with: past the letters, so
/// each token from END to TO has one beneath bit 7, which ends a message.
const FIRST_CODE: u8 = b'Z' + 1;
/// A code's token less the code.
const CODE_BIAS: u8 = token::END - FIRST_CODE;
/// The last token a code reaches.
const LAST_CODED: u8 = CODE_BIAS + 0x7F;

/// `message` with each keyword it spells as that keyword's code, the
/// longest where two start together.
fn encode(message: &str) -> Vec<u8> {
    let mut text = message.as_bytes();
    let mut codes = Vec::new();
    while let [first, rest @ ..] = text {
        let borrowed = (token::END..=LAST_CODED)
            .zip(KEYWORDS)
            .filter(|(_, keyword)| text.starts_with(keyword.as_bytes()))
            .max_by_key(|(_, keyword)| keyword.len());
        if let Some((token, keyword)) = borrowed {
            codes.push(token - CODE_BIAS);
            text = text.get(keyword.len()..).unwrap_or_default();
        } else {
            codes.push(*first);
            text = rest;
        }
    }
    codes
}

/// The flowing code; [`raisers`] is pinned by the module above.
pub fn emit(a: &mut Asm) {
    a.movable(report);
    a.later(texts);
}

fn texts(a: &mut Asm) {
    a.here("error_messages");
    for message in MESSAGES {
        let mut codes = encode(message);
        if let Some(last) = codes.last_mut() {
            *last |= 0x80;
        }
        a.bytes(&codes);
    }
    for (name, text) in [("error_suffix", ERROR_SUFFIX), ("in_text", IN)] {
        a.here(name);
        a.bytes(text.as_bytes());
        a.byte(0);
    }
    a.here("ready_text");
    a.bytes(READY.as_bytes());
    a.here("new_line_text");
    a.bytes(NEW_LINE.as_bytes());
    a.byte(0);
}

const _: () = {
    let mut index = 0;
    while index < MESSAGES.len() {
        assert!(
            !MESSAGES[index].is_empty(),
            "every message has a last character"
        );
        index += 1;
    }
};

/// One entry an error, each loading its number, reading past the others'
/// and going through IERROR.
/// `kernal_check`: after a KERNAL call, C set raises its error A —
/// `kernal_error` — A 0, the STOP key, as BREAK; but RS-232's move of the
/// top of memory is followed, with the files kept open, everything else
/// cleared and the statements after run.
/// ILLEGAL QUANTITY's first, at the chain's start.
pub(super) fn raisers(a: &mut Asm) {
    let [(first, number), rest @ ..] = RAISERS;
    a.here(first);
    a.ldx(imm(number));
    for (name, number) in rest {
        a.skip_over(|a| {
            a.here(name);
            a.ldx(imm(number));
        });
    }
    a.skip_over(|a| {
        a.here("load_error");
        a.ldx(imm(number::LOAD));
    });
    a.here("error");
    a.jmp(abs(vector::IERROR).ind());
    a.here("kernal_check");
    a.bcs("kernal_error");
    a.rts();
    a.here("kernal_error");
    let moved = a.local();
    a.cmp(imm(kernal_error::MEMORY_MOVED));
    a.beq(moved);
    a.tax();
    a.bne("error");
    a.ldx(imm(number::BREAK));
    a.bne("error");
    a.bind(moved);
    a.sec();
    a.jsr(kernal::MEMTOP);
    a.stx(z(zp::MEMSIZ));
    a.sty(z(zp::MEMSIZ + 1));
    a.jsr("clear_open");
    a.jmp("next_statement");
}

/// IERROR's default: output back to the screen, but for BREAK, which leaves
/// it where it is; a new line, `?` and X's message and `  ERROR` — BREAK
/// has neither — and its line in a program, then READY; X with its top bit
/// set is READY alone. The stack is cleared, and an error but BREAK leaves
/// nothing for CONT.
fn report(a: &mut Asm) {
    a.here("error_report");
    let message = a.local();
    let find = a.local();
    let skip = a.local();
    let print = a.local();
    let suffixed = a.local();
    let direct = a.local();
    let kept = a.local();
    a.txa();
    a.bmi("ready");
    a.pha();
    a.cpx(imm(number::BREAK));
    a.beq(kept);
    a.jsr("restore_output");
    a.bind(kept);
    a.lda(imm(lo("new_line_text")));
    a.ldy(imm(hi("new_line_text")));
    a.jsr("print_string");
    a.pla();
    a.pha();
    a.tax();
    a.cpx(imm(number::BREAK));
    a.beq(message);
    a.lda(imm(b'?'));
    a.jsr("print_char");
    // The messages run past a page: INDEX's high byte follows Y over.
    a.bind(message);
    a.lda(imm(lo("error_messages")));
    a.sta(z(zp::INDEX));
    a.lda(imm(hi("error_messages")));
    a.sta(z(zp::INDEX + 1));
    a.ldy(imm(0));
    a.bind(find);
    a.dex();
    a.beq(print);
    a.bind(skip);
    a.lda(z(zp::INDEX).ind_y());
    step(a);
    a.asl(Acc);
    a.bcc(skip);
    a.bcs(find);
    a.bind(print);
    a.tya();
    a.pha();
    a.lda(z(zp::INDEX).ind_y());
    a.pha();
    a.and(imm(0x7F));
    a.jsr("print_glyph");
    a.pla();
    a.tax();
    a.pla();
    a.tay();
    step(a);
    a.txa();
    a.bpl(print);
    a.pla();
    a.cmp(imm(number::BREAK));
    a.beq(suffixed);
    a.lda(imm(0));
    a.sta(z(zp::OLDTXT + 1));
    a.lda(imm(lo("error_suffix")));
    a.ldy(imm(hi("error_suffix")));
    a.jsr("print_string");
    a.bind(suffixed);
    a.jsr("reset_stacks");
    a.lda(z(zp::CURLIN + 1));
    a.cmp(imm(DIRECT_MODE));
    a.beq(direct);
    a.jsr("print_in_line");
    a.bind(direct);
    a.fall_into("ready");

    // READY: the prompt, to the output as it stands, direct mode, then the
    // main loop.
    a.here("ready");
    a.lda(imm(lo("ready_text")));
    a.ldy(imm(hi("ready_text")));
    a.jsr("print_string");
    a.lda(imm(DIRECT_MODE));
    a.sta(z(zp::CURLIN + 1));
    a.lda(imm(CONTROL_MESSAGES));
    a.jsr(kernal::SETMSG);
    a.jmp("main");

    // A message's character, or the keyword its code spells; Y is not kept.
    a.here("print_glyph");
    let borrowed = a.local();
    a.cmp(imm(FIRST_CODE));
    a.bcs(borrowed);
    a.jmp("print_char");
    a.bind(borrowed);
    a.adc(imm(CODE_BIAS - 1));
    a.jmp("keyword");

    // " IN " and the current line.
    a.here("print_in_line");
    a.lda(imm(lo("in_text")));
    a.ldy(imm(hi("in_text")));
    a.jsr("print_string");
    a.lda(z(zp::CURLIN + 1));
    a.ldx(z(zp::CURLIN));
    a.jmp("print_unsigned");
}

/// Y past the message byte at INDEX it indexed, into INDEX's next page.
fn step(a: &mut Asm) {
    let same_page = a.local();
    a.iny();
    a.bne(same_page);
    a.inc(z(zp::INDEX + 1));
    a.bind(same_page);
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use super::{encode, CODE_BIAS, FIRST_CODE, KEYWORDS, MESSAGES};
    use crate::rom::c64::basic::tokens::token;

    #[test]
    fn each_message_spells_itself_back_through_its_keywords() {
        for message in MESSAGES {
            let spelled: String = encode(message)
                .into_iter()
                .map(|code| {
                    if code >= FIRST_CODE {
                        let keyword = usize::from(code + CODE_BIAS - token::END);
                        String::from(KEYWORDS[keyword])
                    } else {
                        String::from(char::from(code))
                    }
                })
                .collect();
            assert_eq!(spelled, message);
        }
        let stored: usize = MESSAGES.iter().map(|message| encode(message).len()).sum();
        assert!(
            stored < MESSAGES.iter().map(|message| message.len()).sum(),
            "codes take less room"
        );
    }
}
