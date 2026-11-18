//! The ROM's frame: the service calls it answers (`*VIM` and `*HELP`), the
//! language entry, the error handler, the main loop and the keys, and
//! leaving for the language the editor was started from.

use super::ops::cmp16;
use super::ws::{page, reading, show, start, zp as z, SURVIVED};
use crate::rom::asm::{abs, byte, hi, imm, lo, zp, Asm, RTS};
use crate::rom::os::{
    entry, osbyte, rom_type, service, vector, zp as os, COPYRIGHT, CR, ROM_TYPES,
};

/// The ROM's type: a language with a service entry.
const ROM_TYPE: u8 = rom_type::SERVICE | rom_type::LANGUAGE | rom_type::CODE;
/// Its version byte, and the version *HELP shows.
const VERSION: u8 = 1;
const VERSION_TEXT: &str = "1.00";
/// Its title, which is also its command.
pub(super) const TITLE: &str = "VIM";
/// ESCAPE's code, while it is a key.
pub(super) const ESCAPE: u8 = 0x1B;
/// The bell.
const BELL: u8 = 7;
/// The keyboard status bits that turn CAPS LOCK and SHIFT LOCK off, since
/// vi's commands are told apart by case.
const LOCKS_OFF: u8 = 0x30;
/// What leaving writes: the windows reset and the screen cleared.
const VDU_RESET: [u8; 2] = [26, 12];

pub(super) fn emit(a: &mut Asm) {
    header(a);
    services(a);
    command_word(a);
    starting(a);
    language(a);
    main_loop(a);
    errors(a);
    keys(a);
    leaving(a);
    messages(a);
}

/// The header: the language and service entries, the type, the copyright's
/// offset and the version, then the title, version and copyright.
fn header(a: &mut Asm) {
    a.jmp("language");
    a.jmp("service");
    a.byte(ROM_TYPE);
    a.byte(lo("copyright"));
    a.byte(VERSION);
    a.text(TITLE);
    a.byte(0);
    a.text(VERSION_TEXT);
    a.here("copyright");
    a.byte(0);
    a.text(COPYRIGHT);
    a.byte(0);
}

/// `*VIM [file]` makes the editor the language, the file named or the text
/// kept; `*HELP` names the ROM, and `*HELP VIM` its command.
fn services(a: &mut Asm) {
    a.here("service");
    a.cmp(imm(service::COMMAND));
    let not_command = a.local();
    a.bne(not_command);
    a.jsr("our_word");
    a.bcs(RTS);
    a.jmp("start_command");
    a.bind(not_command);
    a.cmp(imm(service::HELP));
    a.bne(RTS);
    a.pha();
    a.tya();
    a.pha();
    let spaced = a.here_local();
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(b' '));
    let unspaced = a.local();
    a.bne(unspaced);
    a.iny();
    a.bne(spaced);
    a.bind(unspaced);
    a.cmp(imm(CR));
    let asked = a.local();
    a.bne(asked);
    a.call_with_text("print_inline", &["\rVIM ", VERSION_TEXT, "\r  VIM\r"]);
    let passed = a.local();
    a.jmp(passed);
    a.bind(asked);
    a.lda(imm(service::HELP));
    a.jsr("our_word");
    let ours = a.local();
    a.bcc(ours);
    a.jmp(passed);
    a.bind(ours);
    a.call_with_text(
        "print_inline",
        &["\rVIM ", VERSION_TEXT, "\r  VIM <file>\r  :help in it\r"],
    );
    a.bind(passed);
    a.pla();
    a.tay();
    a.pla();
    a.rts();
}

/// `our_word`: whether the command line names the editor.
fn command_word(a: &mut Asm) {
    // `our_word`: carry clear where the text at (TEXT),Y starts VIM, any
    // case, ending there; Y then past it. A and Y kept otherwise.
    a.here("our_word");
    a.pha();
    a.tya();
    a.pha();
    let differs = a.local();
    for letter in TITLE.bytes() {
        a.lda(zp(os::TEXT).ind_y());
        a.and(imm(0xDF));
        a.cmp(imm(letter));
        a.bne(differs);
        a.iny();
    }
    a.lda(zp(os::TEXT).ind_y());
    a.and(imm(0xDF));
    a.cmp(imm(b'A'));
    let ended = a.local();
    a.bcc(ended);
    a.cmp(imm(b'Z' + 1));
    a.bcc(differs);
    a.bind(ended);
    a.pla();
    a.pla();
    a.clc();
    a.rts();
    a.bind(differs);
    a.pla();
    a.tay();
    a.pla();
    a.sec();
    a.rts();
}

/// `start_command`: `*VIM` taken.
fn starting(a: &mut Asm) {
    // `start_command`: the name after `*VIM` kept, the language left for
    // noted, and the editor entered.
    a.here("start_command");
    a.clc();
    a.jsr(entry::GSINIT);
    a.php();
    a.ldx(imm(0));
    a.plp();
    let named = a.local();
    a.bne(named);
    a.stx(abs(page::START));
    let noted = a.local();
    a.beq(noted);
    a.bind(named);
    let read = a.here_local();
    let read_all = a.local();
    a.jsr(entry::GSREAD);
    a.bcs(read_all);
    a.sta(abs(page::NAME).x());
    a.inx();
    a.cpx(imm(page::NAME_LEN));
    a.bne(read);
    a.bind(read_all);
    a.lda(imm(CR));
    a.sta(abs(page::NAME).x());
    a.lda(imm(start::NAMED));
    a.sta(abs(page::START));
    a.bind(noted);
    a.lda(imm(osbyte::LANGUAGE));
    a.ldx(imm(0));
    a.ldy(imm(0xFF));
    a.jsr(entry::OSBYTE);
    a.cpx(zp(os::ROM));
    let entered_from_us = a.local();
    a.beq(entered_from_us);
    a.stx(abs(page::LEAVE_FOR));
    a.bind(entered_from_us);
    a.lda(imm(osbyte::ENTER_LANGUAGE));
    a.ldx(zp(os::ROM));
    a.jmp(abs(entry::OSBYTE));
}

/// The language entry: the stack, the error handler and the keys taken,
/// the text kept where it survived or made, and the main loop. The MOS
/// enters a language with interrupts disabled, and nothing reads the
/// keyboard until they are enabled.
fn language(a: &mut Asm) {
    a.here("language");
    a.cli();
    a.cld();
    a.ldx(imm(0xFF));
    a.txs();
    a.stx(zp(z::STACK));
    a.lda(imm(lo("on_error")));
    a.sta(abs(vector::BRK));
    a.lda(imm(hi("on_error")));
    a.sta(abs(vector::BRK + 1));
    a.jsr("install");
    a.lda(imm(osbyte::KEYBOARD_STATUS));
    a.ldx(imm(0));
    a.ldy(imm(0xFF));
    a.jsr(entry::OSBYTE);
    a.stx(abs(page::KEYBOARD));
    // No file is open yet: the language before used these pages as its own.
    a.lda(imm(0));
    a.sta(abs(page::HANDLE));
    a.jsr("take_keys");
    a.lda(imm(0xFF));
    a.sta(zp(z::DOT_AT));
    a.lda(imm(0));
    a.sta(zp(z::RECORD));
    a.sta(zp(z::OP));
    a.sta(zp(z::SHOW));
    a.lda(abs(page::START));
    a.cmp(imm(start::NAMED));
    let fresh = a.local();
    a.beq(fresh);
    a.jsr("survived");
    let kept = a.local();
    a.bcc(kept);
    a.bind(fresh);
    a.lda(imm(DEFAULT_MODE));
    a.sta(zp(z::MODE));
    a.jsr("set_mode");
    a.jsr("set_area");
    a.jsr("find_ram");
    a.jsr("buf_reset");
    a.lda(imm(0));
    a.sta(zp(z::FILE));
    a.sta(zp(z::SYN));
    a.sta(zp(z::OPTIONS));
    a.sta(zp(z::DOT_LEN));
    // No search yet and no addresses for a text never read: the language
    // before left its own in these pages.
    a.sta(abs(page::PATTERN));
    a.ldx(imm(7));
    let unaddressed = a.here_local();
    a.sta(abs(page::LOAD).x());
    a.dex();
    a.bpl(unaddressed);
    // The text is kept from here, so an error opening the file leaves an
    // empty text a BREAK keeps.
    a.lda(abs(page::START));
    a.pha();
    a.jsr("keep_text");
    a.pla();
    a.cmp(imm(start::NAMED));
    let unnamed = a.local();
    a.bne(unnamed);
    a.jsr("open_file");
    a.jmp("main");
    a.bind(unnamed);
    a.lda(imm(CR));
    a.sta(abs(page::NAME));
    a.jmp("main");
    a.bind(kept);
    a.jsr("reclaim");
    a.jsr("set_mode");
    a.jsr("keep_text");
    a.call_with_text("say", &["Text kept"]);
    a.fall_into("main");
}

/// The main loop, and the text kept and found again.
fn main_loop(a: &mut Asm) {
    // The main loop: what is owed drawn, a key read and done.
    a.here("main");
    a.ldx(zp(z::STACK));
    a.txs();
    a.lda(imm(0xFF));
    a.sta(zp(z::DOT_AT));
    a.lda(imm(0));
    a.sta(zp(z::OP));
    a.sta(zp(z::COUNT));
    a.sta(zp(z::COUNT + 1));
    a.sta(zp(z::RECORD));
    a.jsr("refresh");
    a.jsr("read_key");
    a.pha();
    a.lda(zp(z::SHOW));
    a.and(imm(!show::MESSAGE));
    a.sta(zp(z::SHOW));
    a.pla();
    a.jsr("normal_key");
    a.jmp("main");

    // `keep_text`: the text marked as kept, a BREAK resuming it.
    a.here("keep_text");
    a.lda(imm(start::RESUME));
    a.sta(abs(page::START));
    for (at, mark) in SURVIVED.iter().enumerate() {
        a.lda(imm(*mark));
        a.sta(abs(page::MARK + u16::from(byte(at))));
    }
    a.rts();

    // `set_area`: the area from OSHWM to HIMEM in the mode chosen.
    a.here("set_area");
    a.lda(imm(osbyte::OSHWM));
    a.jsr(entry::OSBYTE);
    a.stx(zp(z::BUF));
    a.sty(zp(z::BUF + 1));
    a.lda(imm(osbyte::HIMEM));
    a.jsr(entry::OSBYTE);
    a.stx(zp(z::LIMIT));
    a.sty(zp(z::LIMIT + 1));
    a.rts();

    // `survived`: carry clear where the text the workspace describes is
    // still whole: the mark, the area this mode gives, and every boundary
    // in order.
    a.here("survived");
    let lost = a.local();
    for (at, mark) in SURVIVED.iter().enumerate() {
        a.lda(abs(page::MARK + u16::from(byte(at))));
        a.cmp(imm(*mark));
        a.bne(lost);
    }
    a.lda(zp(z::MODE));
    a.cmp(imm(byte(super::screen::MODES.len())));
    a.bcs(lost);
    a.lda(imm(osbyte::OSHWM));
    a.jsr(entry::OSBYTE);
    a.cpx(zp(z::BUF));
    a.bne(lost);
    a.cpy(zp(z::BUF + 1));
    a.bne(lost);
    a.lda(imm(osbyte::HIMEM_MODE));
    a.ldx(zp(z::MODE));
    a.jsr(entry::OSBYTE);
    a.cpx(zp(z::LIMIT));
    a.bne(lost);
    a.cpy(zp(z::LIMIT + 1));
    a.bne(lost);
    for (low, high) in [
        (z::GAP, z::BUF),
        (z::GEND, z::GAP),
        (z::TEND, z::GEND),
        (z::REG, z::TEND),
        (z::LIMIT, z::REG),
        (z::LEN, z::CUR),
    ] {
        cmp16(a, low, high);
        a.bcc(lost);
    }
    a.clc();
    a.rts();
    a.bind(lost);
    a.sec();
    a.rts();
}

/// The mode the editor starts in.
const DEFAULT_MODE: u8 = 7;

/// `on_error`: an OS error shown on the status line, whatever was under
/// way abandoned and the text kept, and the main loop again. The keys and
/// the mode are taken again, since an OS command may have changed either.
fn errors(a: &mut Asm) {
    a.here("on_error");
    a.cli();
    a.cld();
    a.ldx(zp(z::STACK));
    a.txs();
    a.jsr("take_keys");
    a.jsr("set_mode");
    a.jsr("keep_text");
    a.jsr("reclaim");
    // A file whose opening failed is no longer named, so nothing writes the
    // empty text over it.
    a.bit(zp(z::READING));
    let named = a.local();
    a.bpl(named);
    a.lda(imm(CR));
    a.sta(abs(page::NAME));
    a.lda(imm(reading::OPENED));
    a.sta(zp(z::READING));
    a.sta(zp(z::FILE));
    a.bind(named);
    a.ldy(imm(1));
    a.ldx(imm(0));
    let copy = a.here_local();
    let copied = a.local();
    a.lda(zp(crate::rom::os::ERROR_POINTER).ind_y());
    a.beq(copied);
    a.sta(abs(page::MESSAGE).x());
    a.iny();
    a.inx();
    a.cpx(imm(page::MESSAGE_LEN));
    a.bne(copy);
    a.bind(copied);
    a.lda(imm(0));
    a.sta(abs(page::MESSAGE).x());
    a.lda(zp(z::SHOW));
    a.and(imm(!show::INSERT));
    a.ora(imm(show::MESSAGE));
    a.sta(zp(z::SHOW));
    a.jsr("close_file");
    a.jsr("fix_cursor");
    a.jmp("main");

    // `fail`: the bell, the command abandoned.
    a.here("fail");
    a.lda(imm(BELL));
    a.jsr(entry::OSWRCH);
    a.jmp("main");
}

/// The keys: `take_keys` has the cursor keys give codes, ESCAPE a key and
/// the locks off; `give_keys` puts all three back as the editor found them.
/// `read_key`: the next key, from a repeat being played back or the
/// keyboard, kept for `.` while a change is recorded.
fn keys(a: &mut Asm) {
    a.here("take_keys");
    a.lda(imm(LOCKS_OFF));
    a.ldy(imm(!LOCKS_OFF));
    a.ldx(imm(1));
    let set = a.local();
    a.bne(set);
    a.here("give_keys");
    a.lda(abs(page::KEYBOARD));
    a.ldy(imm(0));
    a.ldx(imm(0));
    a.bind(set);
    a.stx(zp(z::T0));
    a.tax();
    a.lda(imm(osbyte::KEYBOARD_STATUS));
    a.jsr(entry::OSBYTE);
    a.lda(imm(osbyte::LOCK_LAMPS));
    a.jsr(entry::OSBYTE);
    a.ldx(zp(z::T0));
    a.lda(imm(osbyte::CURSOR_KEYS));
    a.jsr(entry::OSBYTE);
    a.ldx(zp(z::T0));
    a.lda(imm(osbyte::ESCAPE_KEY));
    a.ldy(imm(0));
    a.jmp(abs(entry::OSBYTE));

    a.here("read_key");
    a.ldx(zp(z::DOT_AT));
    a.inx();
    let keyboard = a.local();
    a.beq(keyboard);
    a.dex();
    a.cpx(zp(z::DOT_LEN));
    let played = a.local();
    a.bcs(played);
    a.lda(abs(page::DOT).x());
    a.inc(zp(z::DOT_AT));
    a.rts();
    a.bind(played);
    a.lda(imm(0xFF));
    a.sta(zp(z::DOT_AT));
    a.bind(keyboard);
    a.jsr("os_key");
    a.ldx(zp(z::RECORD));
    a.beq(RTS);
    a.ldx(zp(z::DOT_LEN));
    a.cpx(imm(page::DOT_LEN));
    let overflow = a.local();
    a.bcs(overflow);
    a.sta(abs(page::DOT).x());
    a.inc(zp(z::DOT_LEN));
    a.rts();
    // A change too long to repeat is not repeated.
    a.bind(overflow);
    a.ldx(imm(0));
    a.stx(zp(z::DOT_LEN));
    a.stx(zp(z::RECORD));
    a.rts();

    // `os_key`: a key from the keyboard; an escape condition, which an OS
    // command may leave, acknowledged and read as ESCAPE.
    a.here("os_key");
    a.jsr(entry::OSRDCH);
    a.bcc(RTS);
    a.lda(imm(osbyte::ACKNOWLEDGE_ESCAPE));
    a.jsr(entry::OSBYTE);
    a.lda(imm(ESCAPE));
    a.rts();
}

/// `leave`: the keys given back, the text no longer kept, and the language
/// the editor was started from entered; refused where that is no language.
fn leaving(a: &mut Asm) {
    a.here("leave");
    a.ldx(abs(page::LEAVE_FOR));
    a.cpx(imm(16));
    let none = a.local();
    a.bcs(none);
    a.cpx(zp(os::ROM));
    a.beq(none);
    a.lda(abs(ROM_TYPES).x());
    a.and(imm(rom_type::LANGUAGE));
    a.beq(none);
    a.jsr("give_keys");
    a.lda(imm(0));
    a.sta(abs(page::MARK));
    for b in VDU_RESET {
        a.lda(imm(b));
        a.jsr(entry::OSWRCH);
    }
    a.ldx(abs(page::LEAVE_FOR));
    a.lda(imm(osbyte::ENTER_LANGUAGE));
    a.jmp(abs(entry::OSBYTE));
    a.bind(none);
    a.call_with_text("say", &["No language"]);
    a.jmp("main");
}

/// `print_inline`: the text after the call printed; `return_past` back to
/// the byte after a text's zero.
fn messages(a: &mut Asm) {
    a.here("print_inline");
    a.pla();
    a.sta(zp(z::P0));
    a.pla();
    a.sta(zp(z::P0 + 1));
    a.ldy(imm(0));
    let print = a.here_local();
    let printed = a.local();
    a.iny();
    a.lda(zp(z::P0).ind_y());
    a.beq(printed);
    a.jsr(entry::OSASCI);
    a.jmp(print);
    a.bind(printed);
    a.fall_into("return_past");

    // `return_past`: back to the byte after the text's zero at (P0),Y.
    a.here("return_past");
    a.tya();
    a.sec();
    a.adc(zp(z::P0));
    a.sta(zp(z::P0));
    let carried = a.local();
    a.bcc(carried);
    a.inc(zp(z::P0 + 1));
    a.bind(carried);
    a.jmp(abs(z::P0).ind());
}
