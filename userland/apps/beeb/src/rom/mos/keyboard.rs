//! The keyboard: KEYV, the matrix scan with two-key rollover and
//! auto-repeat, translation into codes, the soft keys, OSRDCH, INKEY and
//! OSWORD 0's line input with copy editing.
//!
//! The keyboard autoscans while no key is down, a key pressed interrupting
//! on CA2. That interrupt stands down and sets the semaphore at `&242`; the
//! 100 Hz tick then scans the matrix while a key is down or the semaphore is
//! set, and once every key is up arms the interrupt again if the semaphore is
//! still set, clearing it. OSBYTE 178 clearing it meanwhile leaves the
//! keyboard deaf until it is set again, as MOS 1.20 does. The key last
//! pressed is `&EC` and the one before it `&ED`, each its internal number
//! with the top bit set.

use super::ws::{buffer, os, page2, var, vdu, vdu_zp};
use super::{latch, svia, DELETE};
use crate::machine::latch as machine_latch;
use crate::machine::via::{irq, reg};
use crate::rom::asm::{abs, imm, zp, Acc, Asm, RTS};

/// Port B values that drive the latch's keyboard enable bit.
const CPU_SCAN: u8 = latch(machine_latch::KEYBOARD_AUTOSCAN, false);
const AUTOSCAN: u8 = latch(machine_latch::KEYBOARD_AUTOSCAN, true);
/// A key number's top bit: as PA7 reads it, and as OSBYTE &79 and INKEY
/// mark a key down.
pub const KEY_DOWN: u8 = 0x80;
// The tick walks the two key slots as a pair.
const _: () = assert!(os::KEY_FIRST == os::KEY_LAST + 1);
/// Keyboard status bits.
const CAPS_OFF: u8 = 0x10;
const SHIFT_LOCK_OFF: u8 = 0x20;
/// Translation markers for keys that are not characters.
const MARK_CAPS: u8 = 0x01;
const MARK_SHIFT_LOCK: u8 = 0x02;
const MARK_TAB: u8 = 0x03;
const MARK_ESCAPE: u8 = 0x04;
/// The codes the cursor keys and COPY make: soft keys 11 to 15.
const COPY: u8 = 0x8B;
/// The soft keys as MOS 1.20 keeps them at &B00: each key's text as an
/// offset from &B01, then the end of all the text. Text lies in the order it
/// was defined, and a key with none points at the end.
const KEY_POINTERS: u16 = buffer::SOFT_KEYS;
/// The soft keys there are, f0 to f15.
const KEYS: u8 = 16;
const KEY_END: u16 = buffer::SOFT_KEYS + KEYS as u16;
const KEY_BASE: u16 = buffer::SOFT_KEYS + 1;
/// Every pointer's offset with no text defined: the byte after the end's.
const KEY_EMPTY: u8 = 0x10;
const _: () = assert!(KEY_BASE + KEY_EMPTY as u16 == KEY_END + 1);
/// The offset the end may not reach, which leaves room for 238 bytes.
const KEY_LIMIT: u8 = 0xFF;
/// The ESCAPE character's code by default.
const ESCAPE: u8 = 0x1B;
/// *KEY's workspace, in the zero page set aside for *commands: the key, its
/// old text's length and start, and the command line's place.
const KEY: u16 = 0xA8;
const OLD: u16 = 0xA9;
const START: u16 = 0xAA;
const LINE: u16 = 0xAB;

/// The unshifted code of each key, row by row, ten columns a row.
#[rustfmt::skip]
const TRANSLATION: [u8; 80] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    b'Q', b'3', b'4', b'5', 0x84, b'8', 0x87, b'-', b'^', 0x8C,
    0x80, b'W', b'E', b'T', b'7', b'I', b'9', b'0', b'_', 0x8E,
    b'1', b'2', b'D', b'R', b'6', b'U', b'O', b'P', b'[', 0x8F,
    MARK_CAPS, b'A', b'X', b'F', b'Y', b'J', b'K', b'@', b':', 0x0D,
    MARK_SHIFT_LOCK, b'S', b'C', b'G', b'H', b'N', b'L', b';', b']', DELETE,
    MARK_TAB, b'Z', b' ', b'V', b'B', b'M', b',', b'.', b'/', COPY,
    MARK_ESCAPE, 0x81, 0x82, 0x83, 0x85, 0x86, 0x88, 0x89, b'\\', 0x8D,
];

pub fn emit(a: &mut Asm) {
    matrix(a);
    keyv(a);
    tick(a);
    translate(a);
    leds(a);
    rdch(a);
    soft_keys(a);
    line_input(a);
    editing(a);
    a.here("key_translation");
    a.bytes(&TRANSLATION);
}

fn matrix(a: &mut Asm) {
    // A's top bit, and N, set if key X is down.
    a.here("key_down");
    a.php();
    a.sei();
    a.jsr("key_scan_on");
    a.stx(abs(svia(reg::ORA_NH)));
    a.lda(abs(svia(reg::ORA_NH)));
    a.and(imm(KEY_DOWN));
    // A scan's end: the keyboard autoscans again, port A left inputs on
    // PA7, and CA2's flag, which the scan's columns may have raised,
    // cleared; N and Z as A.
    a.here("key_scan_done");
    a.pha();
    a.lda(imm(AUTOSCAN));
    a.sta(abs(svia(reg::ORB)));
    a.lda(imm(irq::CA2));
    a.sta(abs(svia(reg::IFR)));
    a.pla();
    a.plp();
    a.ora(imm(0));
    a.rts();

    // X = the first key down from key X on, skipping the columns past the
    // tenth, or &FF: the whole scan one stretch with interrupts off.
    a.here("key_scan_from");
    a.php();
    a.sei();
    a.jsr("key_scan_on");
    a.here("key_scan_next");
    a.txa();
    a.and(imm(0x0F));
    a.cmp(imm(10));
    a.bcc("key_scan_test");
    // Past the tenth column, C set: the next row's first key.
    a.txa();
    a.ora(imm(0x0F));
    a.adc(imm(0));
    a.bmi("key_scan_none");
    a.tax();
    a.here("key_scan_test");
    a.stx(abs(svia(reg::ORA_NH)));
    a.bit(abs(svia(reg::ORA_NH)));
    a.bmi("key_scan_found");
    a.inx();
    a.bpl("key_scan_next");
    a.here("key_scan_none");
    a.ldx(imm(0xFF));
    a.here("key_scan_found");
    a.txa();
    a.jmp("key_scan_done");

    // The keyboard scanned by the CPU: port A out but PA7.
    a.here("key_scan_on");
    a.lda(imm(CPU_SCAN));
    a.sta(abs(svia(reg::ORB)));
    a.lda(imm(!KEY_DOWN));
    a.sta(abs(svia(reg::DDRA)));
    a.rts();
}

/// KEYV: C and V choose the call.
fn keyv(a: &mut Asm) {
    a.here("keyv_default");
    a.bcc("keyv_test");
    a.bvs("keyboard_tick");
    // C set, V clear: OSBYTE &79's scan from key X, or with X's top bit set
    // a test of the key X & &7F, X's top bit set on return if it is down.
    a.txa();
    a.bpl("keyv_scan");
    a.and(imm(!KEY_DOWN));
    a.tax();
    a.jsr("key_down");
    a.stx(zp(os::SCRATCH + 1));
    a.ora(zp(os::SCRATCH + 1));
    a.tax();
    a.rts();
    a.here("keyv_scan");
    a.jmp("key_scan_from");
    a.here("keyv_test");
    a.bvs("keyboard_interrupt");

    // C and V clear: N if CTRL is down, V if SHIFT is.
    a.here("keyboard_modifiers");
    a.php();
    a.sei();
    a.ldx(imm(1));
    a.jsr("key_down");
    a.sta(zp(os::SCRATCH + 1));
    a.ldx(imm(0));
    a.jsr("key_down");
    a.lsr(Acc);
    a.ora(zp(os::SCRATCH + 1));
    a.sta(zp(os::SCRATCH + 1));
    a.plp();
    a.bit(zp(os::SCRATCH + 1));
    a.rts();
}

/// The 100 Hz tick: releases noticed, a new key taken, auto-repeat.
fn tick(a: &mut Asm) {
    // A key pressed: CA2 stands down until every key is up.
    a.here("keyboard_interrupt");
    a.lda(imm(irq::CA2));
    a.sta(abs(svia(reg::IER)));
    a.sta(abs(svia(reg::IFR)));
    a.lda(imm(0xFF));
    a.sta(abs(var::KEYBOARD_SEMAPHORE));
    a.rts();

    a.here("keyboard_tick");
    // A key let go is forgotten, from either slot.
    a.ldy(imm(1));
    a.here("tick_forget");
    a.lda(abs(os::KEY_LAST).y());
    a.beq("tick_forget_next");
    a.and(imm(!KEY_DOWN));
    a.tax();
    a.jsr("key_down");
    a.bmi("tick_forget_next");
    a.lda(imm(0));
    a.sta(abs(os::KEY_LAST).y());
    a.here("tick_forget_next");
    a.dey();
    a.bpl("tick_forget");
    // A key down that neither slot holds is the new one.
    a.ldx(imm(0x10));
    a.here("tick_scan");
    a.jsr("key_scan_from");
    a.txa();
    a.bmi("tick_no_new_key");
    a.ora(imm(KEY_DOWN));
    a.cmp(zp(os::KEY_LAST));
    a.beq("tick_scan_on");
    a.cmp(zp(os::KEY_FIRST));
    a.bne("tick_new_key");
    a.here("tick_scan_on");
    a.inx();
    a.bpl("tick_scan");
    a.here("tick_no_new_key");
    a.lda(zp(os::KEY_LAST));
    a.bne("tick_repeat");
    a.ora(zp(os::KEY_FIRST));
    a.bne(RTS);
    a.lda(abs(var::KEYBOARD_SEMAPHORE));
    a.beq(RTS);
    a.lda(imm(irq::ANY | irq::CA2));
    a.sta(abs(svia(reg::IER)));
    a.lda(imm(0));
    a.sta(abs(var::KEYBOARD_SEMAPHORE));
    a.rts();
    // Auto-repeat the key held, once its delay and then each period ends;
    // a delay of zero turns it off.
    a.here("tick_repeat");
    a.lda(abs(var::REPEAT_DELAY));
    a.beq(RTS);
    a.dec(zp(os::REPEAT_COUNT));
    a.bne(RTS);
    a.lda(abs(var::REPEAT_PERIOD));
    a.sta(zp(os::REPEAT_COUNT));
    a.lda(zp(os::KEY_LAST));
    a.jmp("key_pressed");
    a.here("tick_new_key");
    a.ldx(zp(os::KEY_LAST));
    a.stx(zp(os::KEY_FIRST));
    a.sta(zp(os::KEY_LAST));
    a.ldx(abs(var::REPEAT_DELAY));
    a.stx(zp(os::REPEAT_COUNT));
    a.jmp("key_pressed");
}

/// A key's code: CAPS and SHIFT LOCK toggle, the rest go to the keyboard
/// buffer shifted, controlled and cased.
fn translate(a: &mut Asm) {
    a.here("key_pressed");
    a.and(imm(!KEY_DOWN));
    // Row × 10 + column.
    a.pha();
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.sta(zp(os::SCRATCH));
    a.asl(Acc);
    a.asl(Acc);
    a.adc(zp(os::SCRATCH));
    a.asl(Acc);
    a.sta(zp(os::SCRATCH));
    a.pla();
    a.and(imm(0x0F));
    a.clc();
    a.adc(zp(os::SCRATCH));
    a.tax();
    a.lda(abs("key_translation").x());
    a.cmp(imm(MARK_CAPS));
    a.bne("key_not_caps");
    a.lda(imm(CAPS_OFF));
    a.bne("key_toggle_lock");
    a.here("key_not_caps");
    a.cmp(imm(MARK_SHIFT_LOCK));
    a.bne("key_not_shift_lock");
    a.lda(imm(SHIFT_LOCK_OFF));
    a.here("key_toggle_lock");
    a.eor(abs(var::KEYBOARD_STATUS));
    a.sta(abs(var::KEYBOARD_STATUS));
    a.jmp("keyboard_leds");
    a.here("key_not_shift_lock");
    a.cmp(imm(MARK_TAB));
    a.bne("key_not_tab");
    a.lda(abs(var::TAB));
    a.jmp("key_insert");
    a.here("key_not_tab");
    a.cmp(imm(MARK_ESCAPE));
    a.bne("key_not_escape");
    a.lda(abs(var::ESCAPE_CHAR));
    a.jmp("key_insert");
    a.here("key_not_escape");
    a.sta(zp(os::SCRATCH));
    a.jsr("keyboard_modifiers");
    a.php();
    a.lda(zp(os::SCRATCH));
    a.bmi("key_function");
    // Letters: upper case under CAPS LOCK, inverted by SHIFT; SHIFT LOCK
    // makes every key shifted.
    a.cmp(imm(b'A'));
    a.bcc("key_symbol");
    a.cmp(imm(b'Z' + 1));
    a.bcs("key_symbol");
    a.plp();
    a.php();
    a.bmi("key_control");
    a.lda(abs(var::KEYBOARD_STATUS));
    a.and(imm(CAPS_OFF | SHIFT_LOCK_OFF));
    a.cmp(imm(CAPS_OFF | SHIFT_LOCK_OFF));
    a.lda(zp(os::SCRATCH));
    a.bcc("key_letter_upper");
    a.ora(imm(0x20));
    a.here("key_letter_upper");
    a.plp();
    a.bvc("key_insert");
    a.eor(imm(0x20));
    a.jmp("key_insert");
    key_symbol(a);
}

/// Symbols shift by their ASCII pairs; 0 and @ do not shift, _ shifts
/// to the pound sign.
fn key_symbol(a: &mut Asm) {
    a.here("key_symbol");
    a.plp();
    a.php();
    a.bmi("key_control");
    a.plp();
    a.bvs("key_shifted");
    a.lda(abs(var::KEYBOARD_STATUS));
    a.and(imm(SHIFT_LOCK_OFF));
    a.bne("key_unshifted");
    a.here("key_shifted");
    a.lda(zp(os::SCRATCH));
    a.cmp(imm(b'_'));
    a.bne("key_shift_pairs");
    a.lda(imm(0x60));
    a.jmp("key_insert");
    a.here("key_shift_pairs");
    a.cmp(imm(0x2C));
    a.bcc("key_unshifted");
    a.cmp(imm(b'0'));
    a.beq("key_unshifted");
    a.cmp(imm(0x40));
    a.bcs("key_shift_brackets");
    a.eor(imm(0x10));
    a.jmp("key_insert");
    a.here("key_shift_brackets");
    a.cmp(imm(0x5B));
    a.bcc("key_unshifted");
    a.cmp(imm(0x5F));
    a.bcs("key_unshifted");
    a.eor(imm(0x20));
    a.jmp("key_insert");
    a.here("key_unshifted");
    a.lda(zp(os::SCRATCH));
    a.jmp("key_insert");
    // CTRL takes &40–&7F to &00–&1F.
    a.here("key_control");
    a.plp();
    a.lda(zp(os::SCRATCH));
    a.cmp(imm(0x40));
    a.bcc("key_insert");
    a.and(imm(0x1F));
    a.jmp("key_insert");
    // Function, cursor and COPY keys: SHIFT adds &10 and CTRL &20.
    a.here("key_function");
    a.plp();
    a.php();
    a.bvc("key_function_unshifted");
    a.clc();
    a.adc(imm(0x10));
    a.here("key_function_unshifted");
    a.plp();
    a.bpl("key_insert");
    a.clc();
    a.adc(imm(0x20));

    // Into the keyboard buffer, unless it is the ESCAPE character and ESCAPE
    // is live, which sets the Escape condition instead.
    a.here("key_insert");
    a.ldx(imm(0));
    a.jmp("input_insert");
}

/// The lock LEDs follow the keyboard status: lit for a lock on.
fn leds(a: &mut Asm) {
    a.here("keyboard_leds");
    // An LED lit with its latch bit low.
    for (lock, dark, lit) in [
        (
            CAPS_OFF,
            const { latch(machine_latch::CAPS_LOCK_LED, true) },
            const { latch(machine_latch::CAPS_LOCK_LED, false) },
        ),
        (
            SHIFT_LOCK_OFF,
            const { latch(machine_latch::SHIFT_LOCK_LED, true) },
            const { latch(machine_latch::SHIFT_LOCK_LED, false) },
        ),
    ] {
        let (lighting, set) = (a.local(), a.local());
        a.lda(abs(var::KEYBOARD_STATUS));
        a.and(imm(lock));
        a.beq(lighting);
        a.lda(imm(dark));
        a.bne(set);
        a.bind(lighting);
        a.lda(imm(lit));
        a.bind(set);
        a.sta(abs(svia(reg::ORB)));
    }
    a.rts();
}

/// OSRDCH: a soft key's text, then an EXEC file, then the input buffer,
/// codes from &80 interpreted as *FX4 and *FX221–228 say. Returns C set and
/// A = &1B for an Escape.
fn rdch(a: &mut Asm) {
    a.here("osrdch_default");
    a.here("nvrdch");
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.here("rdch_next");
    a.jsr("rdch_once");
    a.bvs("rdch_next");
    a.sta(zp(os::TEMP));
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.lda(zp(os::TEMP));
    a.rts();

    // One attempt: V clear with A to return and C set for an Escape; V set
    // to try again.
    a.here("rdch_once");
    a.lda(abs(var::SOFT_KEY_LENGTH));
    a.beq("rdch_not_expanding");
    a.dec(abs(var::SOFT_KEY_LENGTH));
    a.ldx(abs(page2::SOFT_KEY_POINTER));
    a.inc(abs(page2::SOFT_KEY_POINTER));
    a.lda(abs(KEY_BASE).x());
    a.jmp("rdch_return");
    a.here("rdch_not_expanding");
    a.ldy(abs(var::EXEC_HANDLE));
    a.beq("rdch_input");
    a.jsr("osbget");
    a.bcc("rdch_return");
    a.lda(imm(0));
    a.sta(abs(var::EXEC_HANDLE));
    a.jsr("osfind");
    a.here("rdch_input");
    a.bit(zp(os::ESCAPE));
    a.bmi("rdch_escape");
    a.cli();
    a.ldx(abs(var::INPUT_SOURCE));
    a.jsr("buffer_remove");
    a.bcc("rdch_interpret");
    a.jmp("rdch_again");
    a.here("rdch_escape");
    a.lda(imm(ESCAPE));
    a.sec();
    a.clv();
    a.rts();
    a.here("rdch_interpret");
    a.tay();
    a.bpl("rdch_return");
    // Cursor keys and COPY first, as *FX4 says.
    a.cmp(imm(COPY));
    a.bcc("rdch_zones");
    a.cmp(imm(0x90));
    a.bcs("rdch_zones");
    a.ldx(abs(var::CURSOR_EDITING));
    a.beq("rdch_edit");
    a.dex();
    a.bne("rdch_zones");
    a.sec();
    a.sbc(imm(4));
    a.jmp("rdch_return");
    a.here("rdch_edit");
    a.jsr("edit_key");
    a.bcs("rdch_return");
    a.jmp("rdch_again");
    // The eight zones of sixteen codes: ignored, a soft key, or a base.
    a.here("rdch_zones");
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.tax();
    a.lda(abs(var::KEY_ZONES - 8).x());
    a.cpx(imm(0x0C));
    a.bcc("rdch_zone_value");
    a.lda(abs(var::INPUT_ZONES - 12).x());
    a.here("rdch_zone_value");
    a.beq("rdch_again");
    a.cmp(imm(1));
    a.beq("rdch_soft_key");
    a.sta(zp(os::SCRATCH));
    a.tya();
    a.and(imm(0x0F));
    a.clc();
    a.adc(zp(os::SCRATCH));
    a.jmp("rdch_return");
    a.here("rdch_soft_key");
    a.tya();
    a.and(imm(0x0F));
    a.jsr("soft_key_start");
    a.here("rdch_again");
    a.clc();
    a.bit(abs("rdch_v_set"));
    a.rts();
    a.here("rdch_return");
    a.ldx(imm(0));
    a.stx(abs(var::PAGED_LINES));
    a.clc();
    a.clv();
    a.rts();
    a.here("rdch_v_set");
    a.byte(0x40);
}

/// The soft keys: starting one, *KEY, and OSBYTE 18.
fn soft_keys(a: &mut Asm) {
    // Expand soft key A.
    a.here("soft_key_start");
    a.tax();
    a.jsr("key_length");
    a.sta(abs(var::SOFT_KEY_LENGTH));
    a.lda(abs(KEY_POINTERS).x());
    a.sta(abs(page2::SOFT_KEY_POINTER));
    a.rts();

    // A = the length of key X's text: from its start to the nearest pointer
    // above it, the end at most.
    a.here("key_length");
    a.lda(abs(KEY_END));
    a.sta(zp(os::TEMP));
    a.ldy(imm(KEYS - 1));
    a.here("key_length_next");
    a.lda(abs(KEY_POINTERS).y());
    a.cmp(abs(KEY_POINTERS).x());
    a.beq("key_length_skip");
    a.bcc("key_length_skip");
    a.cmp(zp(os::TEMP));
    a.bcs("key_length_skip");
    a.sta(zp(os::TEMP));
    a.here("key_length_skip");
    a.dey();
    a.bpl("key_length_next");
    a.lda(zp(os::TEMP));
    a.sec();
    a.sbc(abs(KEY_POINTERS).x());
    a.rts();

    // *KEY n text: the old text taken out, and the new, read by GSREAD
    // straight into place after the rest.
    a.here("key_command");
    a.jsr("read_decimal");
    a.bcs("key_bad");
    a.cpx(imm(KEYS));
    a.bcs("key_bad");
    a.stx(zp(KEY));
    a.lda(abs(var::SOFT_KEY_LENGTH));
    a.bne("key_in_use");
    a.jsr("skip_comma");
    a.sty(zp(LINE));
    key_remove(a);
    key_gather(a);
}

/// Key KEY's old text out: what follows it moves down over it, and every
/// pointer above it with it. The key is left empty.
fn key_remove(a: &mut Asm) {
    a.jsr("key_length");
    a.sta(zp(OLD));
    a.ldy(abs(KEY_POINTERS).x());
    a.sty(zp(START));
    a.here("key_remove");
    a.cpy(abs(KEY_END));
    a.bcs("key_removed");
    a.tya();
    a.clc();
    a.adc(zp(OLD));
    a.tax();
    a.lda(abs(KEY_BASE).x());
    a.sta(abs(KEY_BASE).y());
    a.iny();
    a.bne("key_remove");
    a.here("key_removed");
    a.ldx(imm(KEYS));
    a.here("key_lower");
    a.lda(abs(KEY_POINTERS).x());
    a.cmp(zp(START));
    a.beq("key_lowered");
    a.bcc("key_lowered");
    a.sbc(zp(OLD));
    a.sta(abs(KEY_POINTERS).x());
    a.here("key_lowered");
    a.dex();
    a.bpl("key_lower");
    a.ldx(zp(KEY));
    a.lda(abs(KEY_END));
    a.sta(abs(KEY_POINTERS).x());
}

/// The new text after the rest, the end kept short of the limit; then every
/// pointer at the old end but the key's moves to the new one.
fn key_gather(a: &mut Asm) {
    a.ldy(zp(LINE));
    a.sec();
    a.jsr("gsinit");
    a.ldx(abs(KEY_END));
    a.here("key_gather");
    a.jsr("gsread");
    a.bcs("key_gathered");
    a.sta(abs(KEY_BASE).x());
    a.inx();
    a.cpx(imm(KEY_LIMIT));
    a.bne("key_gather");
    a.here("key_bad");
    a.brk();
    a.byte(0xFB);
    a.text("Bad key");
    a.byte(0);
    a.here("key_in_use");
    a.brk();
    a.byte(0xFA);
    a.text("Key in use");
    a.byte(0);
    a.here("key_gathered");
    a.ldy(imm(0));
    a.here("key_extend");
    a.cpy(zp(KEY));
    a.beq("key_extended");
    a.lda(abs(KEY_POINTERS).y());
    a.cmp(abs(KEY_END));
    a.bne("key_extended");
    a.txa();
    a.sta(abs(KEY_POINTERS).y());
    a.here("key_extended");
    a.iny();
    a.cpy(imm(KEYS + 1));
    a.bne("key_extend");
    a.rts();

    // OSBYTE 18: every key empty, X and Y left 0 as MOS 1.20 leaves them.
    a.here("osbyte_reset_keys");
    a.ldx(imm(0));
    // Every key empty, X kept.
    a.here("key_reset");
    a.lda(imm(KEY_EMPTY));
    a.ldy(imm(0));
    a.here("key_reset_next");
    a.sta(abs(KEY_POINTERS).y());
    a.iny();
    a.bne("key_reset_next");
    a.sty(abs(var::SOFT_KEY_LENGTH));
    a.rts();
}

/// OSWORD 0: a line read into the caller's buffer, with DELETE, CTRL-U and
/// copy editing; its length, less the CR, returned in Y, and C set if Escape
/// ended it.
fn line_input(a: &mut Asm) {
    a.here("osword_0");
    a.ldy(imm(0));
    a.lda(zp(os::CALL_X).ind_y());
    a.sta(zp(os::LINE_BUFFER));
    a.iny();
    a.lda(zp(os::CALL_X).ind_y());
    a.sta(zp(os::LINE_BUFFER + 1));
    for offset in 0..3u8 {
        a.iny();
        a.lda(zp(os::CALL_X).ind_y());
        a.sta(abs(page2::LINE_LIMITS + u16::from(offset)));
    }
    a.lda(imm(0));
    a.sta(abs(var::PAGED_LINES));
    a.ldy(imm(0));
    a.here("line_next");
    a.jsr("osrdch");
    a.bcs("line_escape");
    a.cmp(imm(DELETE));
    a.beq("line_delete");
    a.cmp(imm(0x15));
    a.beq("line_cancel");
    a.cmp(imm(0x0D));
    a.beq("line_end");
    a.cpy(abs(page2::LINE_LIMITS));
    a.bcs("line_full");
    a.cmp(abs(page2::LINE_LIMITS + 1));
    a.bcc("line_echo");
    a.cmp(abs(page2::LINE_LIMITS + 2));
    a.beq("line_store");
    a.bcs("line_echo");
    a.here("line_store");
    a.sta(zp(os::LINE_BUFFER).ind_y());
    a.iny();
    a.here("line_echo");
    a.jsr("oswrch");
    a.jmp("line_next");
    a.here("line_full");
    a.lda(imm(7));
    a.jsr("oswrch");
    a.jmp("line_next");
    a.here("line_delete");
    a.cpy(imm(0));
    a.beq("line_next");
    a.dey();
    a.jsr("oswrch");
    a.jmp("line_next");
    a.here("line_cancel");
    a.cpy(imm(0));
    a.beq("line_next");
    a.dey();
    a.lda(imm(DELETE));
    a.jsr("oswrch");
    a.jmp("line_cancel");
    a.here("line_end");
    a.sta(zp(os::LINE_BUFFER).ind_y());
    a.jsr("osnewl");
    a.clc();
    a.bcc("line_close");
    a.here("line_escape");
    a.sec();
    a.here("line_close");
    a.php();
    a.sty(zp(os::CALL_Y));
    a.jsr("edit_end");
    a.plp();
    a.rts();
}

/// Copy editing: the cursor keys move the copy cursor away from the input
/// position, and COPY reads the character under it as input.
fn editing(a: &mut Asm) {
    // A = &8C–&8F moves the copy cursor; &8B copies. C set with A a
    // character to deliver, clear for nothing.
    a.here("edit_key");
    a.cmp(imm(COPY));
    a.beq("edit_copy");
    a.pha();
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(super::vdu::status::EDITING));
    a.bne("edit_moving");
    a.jsr("edit_begin");
    a.here("edit_moving");
    a.pla();
    a.cmp(imm(0x8C));
    a.bne("edit_not_left");
    a.jsr("edit_back");
    a.jmp("edit_moved");
    a.here("edit_not_left");
    a.cmp(imm(0x8D));
    a.bne("edit_not_right");
    a.jsr("edit_forward");
    a.jmp("edit_moved");
    a.here("edit_not_right");
    a.cmp(imm(0x8E));
    a.bne("edit_up");
    a.jsr("edit_down");
    a.jmp("edit_moved");
    a.here("edit_up");
    a.jsr("edit_up_row");
    a.here("edit_moved");
    a.jsr("vdu_cursor_update");
    a.clc();
    a.rts();

    a.here("edit_copy");
    a.lda(zp(vdu_zp::STATUS));
    a.and(imm(super::vdu::status::EDITING));
    a.beq("edit_nothing");
    a.ldx(abs(vdu::CURSOR_X));
    a.ldy(abs(vdu::CURSOR_Y));
    a.jsr("read_character");
    a.tax();
    a.beq("edit_nothing");
    a.pha();
    a.jsr("edit_forward");
    a.jsr("vdu_cursor_update");
    a.pla();
    a.sec();
    a.rts();
    a.here("edit_nothing");
    a.lda(imm(7));
    a.jsr("oswrch");
    a.clc();
    a.rts();

    // The copy cursor moves within the text window, wrapping at its edges.
    a.here("edit_back");
    a.lda(abs(vdu::CURSOR_X));
    a.cmp(abs(vdu::TEXT_LEFT));
    a.beq("edit_back_wrap");
    a.bcc("edit_back_wrap");
    a.dec(abs(vdu::CURSOR_X));
    a.rts();
    a.here("edit_back_wrap");
    a.lda(abs(vdu::TEXT_RIGHT));
    a.sta(abs(vdu::CURSOR_X));
    a.here("edit_up_row");
    a.lda(abs(vdu::CURSOR_Y));
    a.cmp(abs(vdu::TEXT_TOP));
    a.beq("edit_up_wrap");
    a.bcc("edit_up_wrap");
    a.dec(abs(vdu::CURSOR_Y));
    a.rts();
    a.here("edit_up_wrap");
    a.lda(abs(vdu::TEXT_BOTTOM));
    a.sta(abs(vdu::CURSOR_Y));
    a.rts();

    a.here("edit_forward");
    a.lda(abs(vdu::CURSOR_X));
    a.cmp(abs(vdu::TEXT_RIGHT));
    a.bcs("edit_forward_wrap");
    a.inc(abs(vdu::CURSOR_X));
    a.rts();
    a.here("edit_forward_wrap");
    a.lda(abs(vdu::TEXT_LEFT));
    a.sta(abs(vdu::CURSOR_X));
    a.here("edit_down");
    a.lda(abs(vdu::CURSOR_Y));
    a.cmp(abs(vdu::TEXT_BOTTOM));
    a.bcs("edit_down_wrap");
    a.inc(abs(vdu::CURSOR_Y));
    a.rts();
    a.here("edit_down_wrap");
    a.lda(abs(vdu::TEXT_TOP));
    a.sta(abs(vdu::CURSOR_Y));
    a.rts();
}
