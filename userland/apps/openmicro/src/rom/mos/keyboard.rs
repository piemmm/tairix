//! The keyboard: KEYV, the matrix scan with two-key rollover and
//! auto-repeat, translation into codes, the soft keys and the Electron's
//! firm keys, OSRDCH, INKEY and OSWORD 0's line input with copy editing.
//!
//! On the Model B the keyboard autoscans while no key is down, a key pressed
//! interrupting on CA2. That interrupt stands down and sets the semaphore at
//! `&242`; the 100 Hz tick then scans the matrix while a key is down or the
//! semaphore is set, and once every key is up arms the interrupt again if the
//! semaphore is still set, clearing it. OSBYTE 178 clearing it meanwhile
//! leaves the keyboard deaf until it is set again, as MOS 1.20 does. The
//! Electron's keyboard has no interrupt: each tick looks while the semaphore
//! is set, and its keys are known by the Model B's internal numbers, as its
//! Advanced User Guide numbers them. The key last pressed is `&EC` and the
//! one before it `&ED`, each its internal number with the top bit set.

use super::ws::{buffer, os, page2, var, vdu, vdu_zp};
use super::{latch, svia, DELETE};
use crate::cpu::STACK_PAGE;
use crate::machine::bbc::keyboard::Key;
use crate::machine::bbc::latch as machine_latch;
use crate::machine::bbc::via::{irq, reg};
use crate::machine::electron::keyboard::{self as electron_keyboard, Key as ElectronKey};
use crate::machine::electron::{control, sheila as ula, KEYBOARD_ROMS};
use crate::machine::{Model, SIDEWAYS_BASE};
use crate::rom::asm::{abs, byte, imm, sym, zp, Acc, Asm, RTS, V_SET};

/// Port B values that drive the latch's keyboard enable bit.
const CPU_SCAN: u8 = latch(machine_latch::KEYBOARD_AUTOSCAN, false);
const AUTOSCAN: u8 = latch(machine_latch::KEYBOARD_AUTOSCAN, true);
/// A key number's top bit: as PA7 reads it, and as OSBYTE &79 and INKEY
/// mark a key down.
pub const KEY_DOWN: u8 = 0x80;
// The tick walks the two key slots as a pair.
const _: () = assert!(os::KEY_FIRST == os::KEY_LAST + 1);
/// Keyboard status bits: SHIFT and CTRL as the last scan found them, the
/// locks, and SHIFT reversing the locks, which SHIFT with CAPS LOCK sets.
const SHIFT_HELD: u8 = 0x08;
const CAPS_OFF: u8 = 0x10;
const SHIFT_LOCK_OFF: u8 = 0x20;
const CTRL_HELD: u8 = 0x40;
const SHIFT_REVERSES: u8 = 0x80;
/// The locks that shift a letter.
const LETTER_LOCKS: u8 = SHIFT_LOCK_OFF | CAPS_OFF;
/// The lock keys, which act as they go down and never repeat.
const CAPS_LOCK_DOWN: u8 = Key::CAPS_LOCK.number() | KEY_DOWN;
const SHIFT_LOCK_DOWN: u8 = Key::SHIFT_LOCK.number() | KEY_DOWN;
/// The table's codes for keys that are not characters, as MOS 1.20 holds
/// them.
const MARK_CAPS: u8 = 0x01;
const MARK_SHIFT_LOCK: u8 = 0x02;
const MARK_TAB: u8 = 0x00;
const MARK_ESCAPE: u8 = 0x1B;
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

/// The matrix's columns, and the first key a scan takes: row 1's, past
/// SHIFT, CTRL and the start-up links.
const COLUMNS: u8 = 10;
const FIRST_SCANNED: u8 = 0x10;
/// The unshifted code of each key from the first scanned, row by row.
#[rustfmt::skip]
const TRANSLATION: [u8; 70] = [
    b'q', b'3', b'4', b'5', 0x84, b'8', 0x87, b'-', b'^', 0x8C,
    0x80, b'w', b'e', b't', b'7', b'i', b'9', b'0', b'_', 0x8E,
    b'1', b'2', b'd', b'r', b'6', b'u', b'o', b'p', b'[', 0x8F,
    MARK_CAPS, b'a', b'x', b'f', b'y', b'j', b'k', b'@', b':', 0x0D,
    MARK_SHIFT_LOCK, b's', b'c', b'g', b'h', b'n', b'l', b';', b']', DELETE,
    MARK_TAB, b'z', b' ', b'v', b'b', b'm', b',', b'.', b'/', COPY,
    MARK_ESCAPE, 0x81, 0x82, 0x83, 0x85, 0x86, 0x88, 0x89, b'\\', 0x8D,
];

/// The Electron's keyboard status bits: CAPS LOCK as the Model B's, and FUNC,
/// SHIFT and CTRL held.
const FUNC_HELD: u8 = 0x20;
const ELECTRON_SHIFT_HELD: u8 = 0x40;
const ELECTRON_CTRL_HELD: u8 = 0x80;
// The tick shifts a key's down bit into FUNC's place.
const _: () = assert!(KEY_DOWN >> 2 == FUNC_HELD);
/// The language entry's reasons that expand a firm key: its length for the
/// key in Y, then its next character in Y.
const FIRM_START: u8 = 3;
const FIRM_NEXT: u8 = 2;
/// The firm keys' codes: FUNC with A–Z, then with the keys typing `:`, `;`,
/// `,`, `-`, `.` and `/`.
const FIRST_FIRM: u8 = 0x90;
const LETTERS: u8 = b'Z' - b'A' + 1;
const FIRM_SYMBOLS: [u8; 6] = *b":;,-./";
/// The keyword each firm key types, in code order: the keycaps' own; the
/// keys whose keycaps carry none type nothing.
const FIRM_KEYWORDS: [&str; 32] = [
    "AUTO", "RENUMBER", "COLOUR", "DRAW", "ELSE", "FOR", "GOTO", "DEG", "INPUT", "RAD", "CHAIN",
    "LIST", "MODE", "NEXT", "OLD\r", "PLOT", "LOCAL", "RUN\r", "STEP", "THEN", "UNTIL", "VDU",
    "RESTORE", "PROC", "REPEAT", "END", "", "", "LOAD", "", "SAVE", "PRINT",
];
// The keyword table is walked by an 8-bit offset.
const _: () = {
    let (mut total, mut n) = (0, 0);
    while n < FIRM_KEYWORDS.len() {
        total += FIRM_KEYWORDS[n].len() + 1;
        n += 1;
    }
    assert!(total < 0x100);
};
/// What the cursor keys and COPY type with SHIFT and with CTRL, from COPY's
/// code on.
const CURSOR_SHIFTED: [u8; 5] = *b"[^|_`";
const CURSOR_CONTROLLED: [u8; 5] = *b"]~\\}{";
/// The codes the cursor keys and COPY make, from COPY's on.
const CURSOR_CODES: u8 = byte(CURSOR_SHIFTED.len());

/// Each Electron key and the Model B key whose internal number it answers
/// to: the one engraved alike, FUNC being CAPS LOCK's.
const ELECTRON_KEYS: [(ElectronKey, Key); 54] = [
    (ElectronKey::RIGHT, Key::RIGHT),
    (ElectronKey::COPY, Key::COPY),
    (ElectronKey::SPACE, Key::SPACE),
    (ElectronKey::LEFT, Key::LEFT),
    (ElectronKey::DOWN, Key::DOWN),
    (ElectronKey::RETURN, Key::RETURN),
    (ElectronKey::DELETE, Key::DELETE),
    (ElectronKey::MINUS, Key::MINUS),
    (ElectronKey::UP, Key::UP),
    (ElectronKey::COLON, Key::COLON),
    (ElectronKey::DIGIT_0, Key::DIGIT_0),
    (ElectronKey::P, Key::P),
    (ElectronKey::SEMICOLON, Key::SEMICOLON),
    (ElectronKey::SLASH, Key::SLASH),
    (ElectronKey::DIGIT_9, Key::DIGIT_9),
    (ElectronKey::O, Key::O),
    (ElectronKey::L, Key::L),
    (ElectronKey::FULL_STOP, Key::FULL_STOP),
    (ElectronKey::DIGIT_8, Key::DIGIT_8),
    (ElectronKey::I, Key::I),
    (ElectronKey::K, Key::K),
    (ElectronKey::COMMA, Key::COMMA),
    (ElectronKey::DIGIT_7, Key::DIGIT_7),
    (ElectronKey::U, Key::U),
    (ElectronKey::J, Key::J),
    (ElectronKey::M, Key::M),
    (ElectronKey::DIGIT_6, Key::DIGIT_6),
    (ElectronKey::Y, Key::Y),
    (ElectronKey::H, Key::H),
    (ElectronKey::N, Key::N),
    (ElectronKey::DIGIT_5, Key::DIGIT_5),
    (ElectronKey::T, Key::T),
    (ElectronKey::G, Key::G),
    (ElectronKey::B, Key::B),
    (ElectronKey::DIGIT_4, Key::DIGIT_4),
    (ElectronKey::R, Key::R),
    (ElectronKey::F, Key::F),
    (ElectronKey::V, Key::V),
    (ElectronKey::DIGIT_3, Key::DIGIT_3),
    (ElectronKey::E, Key::E),
    (ElectronKey::D, Key::D),
    (ElectronKey::C, Key::C),
    (ElectronKey::DIGIT_2, Key::DIGIT_2),
    (ElectronKey::W, Key::W),
    (ElectronKey::S, Key::S),
    (ElectronKey::X, Key::X),
    (ElectronKey::DIGIT_1, Key::DIGIT_1),
    (ElectronKey::Q, Key::Q),
    (ElectronKey::A, Key::A),
    (ElectronKey::Z, Key::Z),
    (ElectronKey::ESCAPE, Key::ESCAPE),
    (ElectronKey::FUNC, Key::CAPS_LOCK),
    (ElectronKey::CTRL, Key::CTRL),
    (ElectronKey::SHIFT, Key::SHIFT),
];

pub fn emit(a: &mut Asm, model: Model) {
    match model {
        Model::ModelB => matrix(a),
        Model::Electron => electron_matrix(a),
    }
    keyv(a, model);
    tick(a, model);
    match model {
        Model::ModelB => translate(a),
        Model::Electron => electron_translate(a),
    }
    leds(a, model);
    rdch(a, model);
    soft_keys(a);
    if model == Model::Electron {
        firm_keys(a);
    }
    // Its branches reach across the whole of it.
    a.whole(line_input);
    editing(a);
    // Indexed by key number, as software indexes it from OSBYTE &AC's
    // address: rows sixteen keys apart, the slots past each row's tenth
    // left to held tables.
    a.whole(|a| {
        a.here("key_translation");
        let rows = TRANSLATION.chunks(usize::from(COLUMNS));
        let last = rows.len() - 1;
        for (row, keys) in rows.enumerate() {
            a.bytes(keys);
            if row < last {
                a.room(usize::from(ROW_STRIDE - COLUMNS));
            }
        }
    });
}

/// Key numbers' rows lie sixteen apart, a row in a number's high nibble.
const ROW_STRIDE: u8 = 0x10;

/// The address OSBYTE &AC and &AD give: the table less the first key
/// number, so adding a key's number reaches its code.
pub(super) fn key_table() -> crate::rom::asm::Addr {
    sym("key_translation") + -i32::from(FIRST_SCANNED)
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
    // tenth, or &FF: the whole scan one stretch with interrupts off. Row 0's
    // SHIFT, CTRL and links are never found, as MOS 1.20's scan finds none.
    a.here("key_scan_from");
    a.php();
    a.sei();
    a.jsr("key_scan_on");
    a.cpx(imm(FIRST_SCANNED));
    a.bcs("key_scan_next");
    a.ldx(imm(FIRST_SCANNED));
    a.here("key_scan_next");
    a.txa();
    a.and(imm(0x0F));
    a.cmp(imm(COLUMNS));
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

/// The Electron's keyboard lines, and what ROM 8's window reads with none
/// selected: a line is selected by its address bit low.
const ELECTRON_LINES: u8 = electron_keyboard::LINES.to_le_bytes()[0];
const _: () = assert!(electron_keyboard::LINES < 0x100);
const NO_LINE: u16 = SIDEWAYS_BASE | ((1 << electron_keyboard::LINES) - 1);
/// The lines the window's low address byte selects.
const LOW_LINES: u8 = 8;
/// The last line, ESCAPE's and FUNC's, read alone, and every line below it
/// read together.
const LAST_LINE_ALONE: u16 = NO_LINE & !(1 << (ELECTRON_LINES - 1));
const BELOW_LAST_LINE: u16 = SIDEWAYS_BASE | (1 << (ELECTRON_LINES - 1));
/// A key's place: its line in the low nibble and its bit in the high.
const PLACE_LINE: u8 = 0x0F;
const PLACE_BIT: u32 = 4;

/// The Electron's keys, ascending by internal number: each one's number, and
/// its place.
fn electron_keys() -> ([u8; 54], [u8; 54]) {
    let mut keys = ELECTRON_KEYS;
    keys.sort_by_key(|(_, key)| key.number());
    let numbers = keys.map(|(_, key)| key.number());
    let places =
        keys.map(|(electron, _)| (1 << (u32::from(electron.bit()) + PLACE_BIT)) | electron.line());
    (numbers, places)
}

/// The Electron's matrix, read through ROM 8 with interrupts off, so none
/// pages another ROM over it meanwhile. The IRQ scratch at &FA is the
/// pointer a line is read through, so it is not kept; X and Y are.
fn electron_matrix(a: &mut Asm) {
    let (numbers, places) = electron_keys();
    let count = byte(numbers.len());
    electron_key_down(a, count);
    electron_key_scan(a, count);
    electron_lines(a);
    a.later(move |a| {
        a.here("electron_key_numbers");
        a.bytes(&numbers);
    });
    a.later(move |a| {
        a.here("electron_key_places");
        a.bytes(&places);
    });
}

/// A key's top bit, and N, set if key X is down; and the end every scan
/// shares.
fn electron_key_down(a: &mut Asm, count: u8) {
    let scratch = os::SCRATCH;
    a.here("key_down");
    a.php();
    a.sei();
    a.txa();
    a.pha();
    a.ldx(imm(0));
    a.here("key_down_find");
    a.cmp(abs("electron_key_numbers").x());
    a.beq("key_down_found");
    a.inx();
    a.cpx(imm(count));
    a.bne("key_down_find");
    a.lda(imm(0));
    a.beq("key_down_out");
    a.here("key_down_found");
    a.lda(abs("electron_key_places").x());
    a.pha();
    a.and(imm(PLACE_LINE));
    a.tax();
    a.jsr("key_line");
    a.sta(zp(scratch));
    a.pla();
    for _ in 0..PLACE_BIT {
        a.lsr(Acc);
    }
    a.and(zp(scratch));
    a.beq("key_down_out");
    a.lda(imm(KEY_DOWN));
    a.here("key_down_out");
    a.sta(zp(scratch));
    a.pla();
    a.tax();
    a.lda(zp(scratch));
    // A scan's end: the ROM paged before put back; N and Z as A.
    a.here("key_scan_done");
    a.jsr("page_f4");
    a.plp();
    a.ora(imm(0));
    a.rts();
}

/// X = the first key down from key X on, or &FF: each line read once, onto
/// the stack, then the keys walked in number order.
fn electron_key_scan(a: &mut Asm, count: u8) {
    let scratch = os::SCRATCH;
    a.here("key_scan_from");
    a.php();
    a.sei();
    a.tya();
    a.pha();
    a.txa();
    a.pha();
    a.ldx(imm(ELECTRON_LINES - 1));
    a.here("key_scan_read");
    a.jsr("key_line");
    a.pha();
    a.dex();
    a.bpl("key_scan_read");
    // Line n lies at &101 + S + n, the first number wanted above them.
    a.tsx();
    a.stx(zp(scratch));
    a.lda(abs(STACK_PAGE + 1 + u16::from(ELECTRON_LINES)).x());
    a.sta(zp(scratch + 1));
    a.ldy(imm(0));
    a.here("key_scan_next");
    a.lda(abs("electron_key_numbers").y());
    a.cmp(zp(scratch + 1));
    a.bcc("key_scan_skip");
    a.lda(abs("electron_key_places").y());
    a.and(imm(PLACE_LINE));
    a.clc();
    a.adc(zp(scratch));
    a.tax();
    a.lda(abs("electron_key_places").y());
    for _ in 0..PLACE_BIT {
        a.lsr(Acc);
    }
    a.and(abs(STACK_PAGE + 1).x());
    a.bne("key_scan_found");
    a.here("key_scan_skip");
    a.iny();
    a.cpy(imm(count));
    a.bne("key_scan_next");
    a.lda(imm(0xFF));
    a.bne("key_scan_drop");
    a.here("key_scan_found");
    a.lda(abs("electron_key_numbers").y());
    // The lines and the first number off the stack, the caller's Y back.
    a.here("key_scan_drop");
    a.sta(zp(scratch + 1));
    a.lda(zp(scratch));
    a.clc();
    a.adc(imm(ELECTRON_LINES + 1));
    a.tax();
    a.txs();
    a.pla();
    a.tay();
    a.ldx(zp(scratch + 1));
    a.txa();
    a.jmp("key_scan_done");
}

/// A line's keys, and whether any key the scan takes is down.
fn electron_lines(a: &mut Asm) {
    let scratch = os::SCRATCH;
    let [no_line_low, no_line_high] = NO_LINE.to_le_bytes();
    // A = line X's keys, ROM 8 paged in; X and Y kept.
    a.here("key_line");
    a.lda(imm(*KEYBOARD_ROMS.start()));
    a.sta(abs(ula::PAGING));
    a.lda(imm(no_line_low));
    a.sta(zp(scratch));
    a.lda(imm(no_line_high));
    a.sta(zp(scratch + 1));
    a.cpx(imm(LOW_LINES));
    a.bcs("key_line_high");
    a.lda(abs("key_line_bits").x());
    a.sta(zp(scratch));
    a.bne("key_line_read");
    a.here("key_line_high");
    a.lda(abs(sym("key_line_bits") + -i32::from(LOW_LINES)).x());
    a.and(imm(no_line_high));
    a.sta(zp(scratch + 1));
    a.here("key_line_read");
    a.tya();
    a.pha();
    a.ldy(imm(0));
    a.lda(zp(scratch).ind_y());
    a.and(imm(electron_keyboard::LINE_KEYS));
    a.sta(zp(scratch));
    a.pla();
    a.tay();
    a.lda(zp(scratch));
    a.rts();

    // Z clear if a key the scan takes is down, SHIFT and CTRL aside: ESCAPE
    // and FUNC are the last line's first two keys.
    a.here("keyboard_any");
    a.php();
    a.sei();
    a.lda(imm(*KEYBOARD_ROMS.start()));
    a.sta(abs(ula::PAGING));
    a.lda(abs(LAST_LINE_ALONE));
    a.and(imm(
        (1 << ElectronKey::ESCAPE.bit()) | (1 << ElectronKey::FUNC.bit())
    ));
    a.ora(abs(BELOW_LAST_LINE));
    a.and(imm(electron_keyboard::LINE_KEYS));
    a.jsr("page_f4");
    a.tax();
    a.plp();
    a.txa();
    a.rts();

    a.later(|a| {
        a.here("key_line_bits");
        for line in 0..LOW_LINES {
            a.byte(!(1u8 << line));
        }
    });
}

/// KEYV: C and V choose the call.
fn keyv(a: &mut Asm, model: Model) {
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

    // C and V clear: N if CTRL is down, V if SHIFT is. The Electron's
    // matrix reads through the scratch, so CTRL waits on the stack.
    a.here("keyboard_modifiers");
    a.php();
    a.sei();
    a.ldx(imm(Key::CTRL.number()));
    a.jsr("key_down");
    match model {
        Model::ModelB => {
            a.sta(zp(os::SCRATCH + 1));
            a.ldx(imm(Key::SHIFT.number()));
            a.jsr("key_down");
            a.lsr(Acc);
        }
        Model::Electron => {
            a.pha();
            a.ldx(imm(Key::SHIFT.number()));
            a.jsr("key_down");
            a.lsr(Acc);
            a.sta(zp(os::SCRATCH + 1));
            a.pla();
        }
    }
    a.ora(zp(os::SCRATCH + 1));
    a.sta(zp(os::SCRATCH + 1));
    a.plp();
    a.bit(zp(os::SCRATCH + 1));
    a.rts();
}

/// The 100 Hz tick: releases noticed, a new key taken, auto-repeat. Each
/// scan records SHIFT and CTRL in the keyboard status, and on the Electron
/// FUNC too.
fn tick(a: &mut Asm, model: Model) {
    match model {
        Model::ModelB => {
            // A key pressed: CA2 stands down until every key is up.
            a.here("keyboard_interrupt");
            a.lda(imm(irq::CA2));
            a.sta(abs(svia(reg::IER)));
            a.sta(abs(svia(reg::IFR)));
            a.lda(imm(0xFF));
            a.sta(abs(var::KEYBOARD_SEMAPHORE));
            a.rts();
        }
        // No key interrupts.
        Model::Electron => {
            a.here("keyboard_interrupt");
            a.rts();
        }
    }

    a.here("keyboard_tick");
    tick_status(a, model);
    tick_keys(a, model);
}

/// The scan's SHIFT and CTRL into the keyboard status, and on the Electron
/// FUNC.
fn tick_status(a: &mut Asm, model: Model) {
    let (shift, ctrl, kept) = match model {
        Model::ModelB => (SHIFT_HELD, CTRL_HELD, !(SHIFT_HELD | CTRL_HELD)),
        Model::Electron => (ELECTRON_SHIFT_HELD, ELECTRON_CTRL_HELD, CAPS_OFF),
    };
    a.jsr("keyboard_modifiers");
    a.php();
    if model == Model::Electron {
        // FUNC, CAPS LOCK's key, as status bit 5.
        a.ldx(imm(Key::CAPS_LOCK.number()));
        a.jsr("key_down");
        a.lsr(Acc);
        a.lsr(Acc);
        a.sta(zp(os::SCRATCH + 1));
    }
    a.lda(abs(var::KEYBOARD_STATUS));
    a.and(imm(kept));
    if model == Model::Electron {
        a.ora(zp(os::SCRATCH + 1));
    }
    a.plp();
    let no_ctrl = a.local();
    a.bpl(no_ctrl);
    a.ora(imm(ctrl));
    a.bind(no_ctrl);
    let no_shift = a.local();
    a.bvc(no_shift);
    a.ora(imm(shift));
    a.bind(no_shift);
    a.sta(abs(var::KEYBOARD_STATUS));
}

/// Releases noticed, a new key taken, auto-repeat.
fn tick_keys(a: &mut Asm, model: Model) {
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
    a.ldx(imm(FIRST_SCANNED));
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
    if model == Model::ModelB {
        a.ora(zp(os::KEY_FIRST));
        a.bne(RTS);
        a.lda(abs(var::KEYBOARD_SEMAPHORE));
        a.beq(RTS);
        a.lda(imm(irq::ANY | irq::CA2));
        a.sta(abs(svia(reg::IER)));
        a.lda(imm(0));
        a.sta(abs(var::KEYBOARD_SEMAPHORE));
    }
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
    a.cmp(imm(CAPS_LOCK_DOWN));
    a.beq(RTS);
    a.cmp(imm(SHIFT_LOCK_DOWN));
    a.beq(RTS);
    a.bne("key_pressed");
    a.here("tick_new_key");
    a.ldx(zp(os::KEY_LAST));
    a.stx(zp(os::KEY_FIRST));
    a.sta(zp(os::KEY_LAST));
    a.ldx(abs(var::REPEAT_DELAY));
    a.stx(zp(os::REPEAT_COUNT));
    a.fall_into("key_pressed");
}

/// A key's code: the lock keys set the locks, the rest go to the keyboard
/// buffer shifted, cased and controlled, as MOS 1.20 does.
fn translate(a: &mut Asm) {
    a.here("key_pressed");
    key_code(a);
    locks(a);
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
    shifting(a);
    shifted(a);
}

/// A = the unshifted code of the key A numbers, its top bit set.
fn key_code(a: &mut Asm) {
    a.and(imm(!KEY_DOWN));
    a.tax();
    a.lda(abs(key_table()).x());
}

/// An Electron key's code. FUNC alone types nothing, and with SHIFT turns
/// CAPS LOCK over. With FUNC a digit is its soft key, &B0 on with SHIFT and
/// CTRL as well, and a letter or one of the keys typing `:`, `;`, `,`, `-`,
/// `.` and `/` its firm key.
fn electron_translate(a: &mut Asm) {
    let status = abs(var::KEYBOARD_STATUS);
    let code = zp(os::SCRATCH);
    a.here("key_pressed");
    a.cmp(imm(Key::CAPS_LOCK.number() | KEY_DOWN));
    a.bne("key_not_func");
    a.lda(status);
    a.and(imm(ELECTRON_SHIFT_HELD));
    a.beq(RTS);
    a.lda(status);
    a.eor(imm(CAPS_OFF));
    a.sta(status);
    a.jmp("keyboard_leds");
    a.here("key_not_func");
    key_code(a);
    a.cmp(imm(MARK_ESCAPE));
    a.bne("key_not_escape");
    a.lda(abs(var::ESCAPE_CHAR));
    a.jmp("key_insert");
    a.here("key_not_escape");
    a.sta(code);
    a.lda(status);
    a.and(imm(FUNC_HELD));
    a.beq("key_unfunced");
    a.lda(code);
    a.cmp(imm(b'0'));
    a.bcc("key_func_symbol");
    a.cmp(imm(b'9' + 1));
    a.bcs("key_func_letter");
    a.and(imm(0x0F));
    a.ora(imm(0x80));
    a.sta(code);
    a.lda(status);
    a.and(imm(ELECTRON_SHIFT_HELD | ELECTRON_CTRL_HELD));
    a.cmp(imm(ELECTRON_SHIFT_HELD | ELECTRON_CTRL_HELD));
    a.lda(code);
    a.bcc("key_func_soft");
    a.ora(imm(0x30));
    a.here("key_func_soft");
    a.jmp("key_insert");
    a.here("key_func_letter");
    a.cmp(imm(b'a'));
    a.bcc("key_func_symbol");
    a.cmp(imm(b'z' + 1));
    a.bcs("key_func_symbol");
    a.adc(imm(FIRST_FIRM - b'a'));
    a.jmp("key_insert");
    a.here("key_func_symbol");
    a.ldx(imm(byte(FIRM_SYMBOLS.len()) - 1));
    a.here("key_func_symbol_next");
    a.cmp(abs("firm_symbols").x());
    a.beq("key_func_symbol_found");
    a.dex();
    a.bpl("key_func_symbol_next");
    a.bmi("key_unfunced");
    a.here("key_func_symbol_found");
    a.txa();
    a.clc();
    a.adc(imm(FIRST_FIRM + LETTERS));
    a.jmp("key_insert");
    electron_character(a);
}

/// An Electron key's code without FUNC: the cursor keys and COPY type their
/// characters with SHIFT or CTRL; a letter is a capital while CAPS LOCK is on
/// or SHIFT is held, a digit or symbol shifted with SHIFT, 0 to `@`; then
/// CTRL takes &40–&7F to &00–&1F.
fn electron_character(a: &mut Asm) {
    let status = abs(var::KEYBOARD_STATUS);
    let code = zp(os::SCRATCH);
    a.here("key_unfunced");
    a.lda(code);
    a.cmp(imm(COPY));
    a.bcc("key_case");
    a.cmp(imm(COPY + CURSOR_CODES));
    a.bcs("key_case");
    a.tax();
    a.lda(status);
    a.and(imm(ELECTRON_SHIFT_HELD));
    a.beq("key_cursor_unshifted");
    a.lda(abs(sym("cursor_shifted") + -i32::from(COPY)).x());
    a.jmp("key_insert");
    a.here("key_cursor_unshifted");
    a.lda(status);
    a.and(imm(ELECTRON_CTRL_HELD));
    a.beq("key_cursor_plain");
    a.lda(abs(sym("cursor_controlled") + -i32::from(COPY)).x());
    a.jmp("key_insert");
    a.here("key_cursor_plain");
    a.txa();
    a.jmp("key_insert");

    a.here("key_case");
    a.cmp(imm(b'a'));
    a.bcc("key_symbol");
    a.cmp(imm(b'z' + 1));
    a.bcs("key_symbol");
    a.tax();
    a.lda(status);
    a.and(imm(CAPS_OFF));
    a.beq("key_capital");
    a.lda(status);
    a.and(imm(ELECTRON_SHIFT_HELD));
    a.bne("key_capital");
    a.txa();
    a.bne("key_ctrl");
    a.here("key_capital");
    a.txa();
    a.and(imm(!0x20));
    a.bne("key_ctrl");
    a.here("key_symbol");
    a.tax();
    a.lda(status);
    a.and(imm(ELECTRON_SHIFT_HELD));
    a.beq("key_symbol_plain");
    a.txa();
    a.cmp(imm(b'0'));
    a.bne("key_symbol_pairs");
    a.lda(imm(b'@'));
    a.bne("key_ctrl");
    a.here("key_symbol_pairs");
    a.cmp(imm(0x2C));
    a.bcc("key_ctrl");
    a.cmp(imm(0x40));
    a.bcs("key_ctrl");
    a.eor(imm(0x10));
    a.bne("key_ctrl");
    a.here("key_symbol_plain");
    a.txa();
    a.here("key_ctrl");
    a.tax();
    a.lda(status);
    a.and(imm(ELECTRON_CTRL_HELD));
    a.beq("key_ctrl_none");
    a.txa();
    a.cmp(imm(0x40));
    a.bcc("key_insert");
    a.and(imm(0x1F));
    a.jmp("key_insert");
    a.here("key_ctrl_none");
    a.txa();

    a.here("key_insert");
    a.ldx(imm(0));
    a.jmp("input_insert");
}

/// CAPS LOCK turns SHIFT LOCK off and, with SHIFT, CAPS LOCK on with SHIFT
/// reversing it, or without, CAPS LOCK over; SHIFT LOCK turns CAPS LOCK off
/// and itself over. Either ends SHIFT reversing them but as CAPS LOCK sets
/// it.
fn locks(a: &mut Asm) {
    a.cmp(imm(MARK_CAPS));
    let not_caps = a.local();
    a.bne(not_caps);
    a.jsr("keyboard_modifiers");
    a.lda(abs(var::KEYBOARD_STATUS));
    let plain = a.local();
    a.bvc(plain);
    a.and(imm(!CAPS_OFF));
    a.ora(imm(SHIFT_REVERSES | SHIFT_LOCK_OFF));
    a.bne("key_locks_set");
    a.bind(plain);
    a.eor(imm(CAPS_OFF));
    a.and(imm(!SHIFT_REVERSES));
    a.ora(imm(SHIFT_LOCK_OFF));
    a.bne("key_locks_set");
    a.bind(not_caps);
    a.cmp(imm(MARK_SHIFT_LOCK));
    a.bne("key_not_shift_lock");
    a.lda(abs(var::KEYBOARD_STATUS));
    a.eor(imm(SHIFT_LOCK_OFF));
    a.and(imm(!SHIFT_REVERSES));
    a.ora(imm(CAPS_OFF));
    a.here("key_locks_set");
    a.sta(abs(var::KEYBOARD_STATUS));
    a.jmp("keyboard_leds");
}

/// Whether the key's code at SCRATCH is shifted, C clear if it is, with
/// CTRL and SHIFT on the stack: the locks that shift it, SHIFT LOCK and for
/// a letter CAPS LOCK, shift it, and SHIFT shifts it too or, while SHIFT
/// reverses the locks, turns that over.
fn shifting(a: &mut Asm) {
    a.ldx(imm(SHIFT_LOCK_OFF));
    a.lda(zp(os::SCRATCH));
    let locks = a.local();
    a.cmp(imm(b'a'));
    a.bcc(locks);
    a.cmp(imm(b'z' + 1));
    a.bcs(locks);
    a.ldx(imm(LETTER_LOCKS));
    a.bind(locks);
    a.stx(zp(os::SCRATCH + 1));
    a.lda(abs(var::KEYBOARD_STATUS));
    a.and(zp(os::SCRATCH + 1));
    // C set if every lock that shifts it is off; V is still SHIFT.
    a.cmp(zp(os::SCRATCH + 1));
    let decided = a.local();
    a.bvc(decided);
    a.bit(abs(var::KEYBOARD_STATUS));
    let shifts = a.local();
    a.bpl(shifts);
    let was_shifted = a.local();
    a.bcc(was_shifted);
    a.bind(shifts);
    a.clc();
    a.bcc(decided);
    a.bind(was_shifted);
    a.sec();
    a.bind(decided);
}

/// The code with C clear shifted: a letter, lower case in the table, upper
/// case; a symbol by its ASCII pair, but 0 and @, and _ to the pound sign; a
/// function key &10 on. Then CTRL takes &40–&7F to &00–&1F, or a function key &20 on.
fn shifted(a: &mut Asm) {
    a.lda(zp(os::SCRATCH));
    a.bmi("key_function");
    let unshifted = a.local();
    a.bcs(unshifted);
    a.cmp(imm(b'_'));
    let pairs = a.local();
    a.bne(pairs);
    a.lda(imm(0x60));
    a.bne("key_control");
    a.bind(pairs);
    a.cmp(imm(0x2C));
    a.bcc("key_control");
    a.cmp(imm(b'0'));
    a.beq("key_control");
    a.cmp(imm(0x40));
    let brackets = a.local();
    a.bcs(brackets);
    a.eor(imm(0x10));
    a.bcc("key_control");
    a.bind(brackets);
    a.cmp(imm(0x5B));
    a.bcc("key_control");
    a.cmp(imm(b'z' + 1));
    a.bcs("key_control");
    a.eor(imm(0x20));
    a.bne("key_control");
    a.bind(unshifted);
    a.here("key_control");
    a.plp();
    a.bpl("key_insert");
    a.cmp(imm(0x40));
    a.bcc("key_insert");
    a.and(imm(0x1F));
    a.jmp("key_insert");

    a.here("key_function");
    let unshifted = a.local();
    a.bcs(unshifted);
    a.adc(imm(0x10));
    a.bind(unshifted);
    a.plp();
    a.bpl("key_insert");
    a.ora(imm(0x20));

    // Into the keyboard buffer, unless it is the ESCAPE character and ESCAPE
    // is live, which sets the Escape condition instead.
    a.here("key_insert");
    a.ldx(imm(0));
    a.jmp("input_insert");
}

/// The lock LEDs follow the keyboard status: lit for a lock on. The
/// Electron's one, CAPS LOCK's, is `&FE07`'s top bit.
fn leds(a: &mut Asm, model: Model) {
    a.here("keyboard_leds");
    if model == Model::Electron {
        a.ldx(imm(!control::CAPS_LOCK));
        a.lda(abs(var::KEYBOARD_STATUS));
        a.and(imm(CAPS_OFF));
        a.eor(imm(CAPS_OFF));
        for _ in 0..3 {
            a.asl(Acc);
        }
        a.jmp("ula_control");
        return;
    }
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

/// RDCHV's default: a character, X and Y kept.
pub(super) fn osrdch_default(a: &mut Asm) {
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
}

/// OSRDCH: a soft key's text or the Electron's firm key's, then an EXEC
/// file, then the input buffer, codes from &80 interpreted as *FX4 and
/// *FX221–228 say. Returns C set and A = &1B for an Escape.
fn rdch(a: &mut Asm, model: Model) {
    // One attempt: V clear with A to return and C set for an Escape; V set
    // to try again.
    a.here("rdch_once");
    if model == Model::Electron {
        a.lda(abs(var::FIRM_LENGTH));
        a.beq("rdch_not_firm");
        a.jsr("firm_key_next");
        a.jmp("rdch_return");
        a.here("rdch_not_firm");
    }
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
    a.jsr("interrupt_window");
    a.ldx(abs(var::INPUT_SOURCE));
    a.jsr("buffer_remove");
    a.bcc("rdch_interpret");
    a.bcs("rdch_again");
    a.here("rdch_escape");
    a.lda(imm(ESCAPE));
    a.sec();
    a.clv();
    a.rts();
    rdch_codes(a, model);
}

/// A code from the input: the cursor keys and COPY as *FX4 says, and each of
/// the zones above them as OSBYTE &DD to &E4 set it.
fn rdch_codes(a: &mut Asm, model: Model) {
    a.here("rdch_interpret");
    a.tay();
    a.bpl("rdch_return");
    a.cmp(imm(COPY));
    a.bcc("rdch_zones");
    a.cmp(imm(COPY + CURSOR_CODES));
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
    a.bcc("rdch_again");
    // The eight zones of sixteen codes: ignored, a soft key, or a base.
    a.here("rdch_zones");
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.lsr(Acc);
    a.tax();
    a.cpx(imm(0x0C));
    a.lda(abs(var::KEY_ZONES - 8).x());
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
    if model == Model::Electron {
        // A firm key's zone expands its keyword.
        a.cmp(imm(FIRST_FIRM));
        a.bcc("rdch_soft_key_start");
        a.cmp(imm(FIRST_FIRM + byte(FIRM_KEYWORDS.len())));
        a.bcs("rdch_soft_key_start");
        a.jsr("firm_key_start");
        a.jmp("rdch_again");
        a.here("rdch_soft_key_start");
    }
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
    a.byte(V_SET);
}

/// The Electron's firm keys: the language expands them if its type byte
/// says it does, through its entry point, else the OS from its own table of
/// the keycaps' keywords.
fn firm_keys(a: &mut Asm) {
    // Begin firm key A.
    a.here("firm_key_start");
    a.tay();
    a.jsr("firm_by_language");
    a.beq("firm_own_start");
    a.lda(imm(FIRM_START));
    a.jsr("language_call");
    a.sty(abs(var::FIRM_LENGTH));
    a.rts();
    a.here("firm_own_start");
    a.tya();
    a.sec();
    a.sbc(imm(FIRST_FIRM));
    a.tax();
    a.ldy(imm(0));
    a.here("firm_own_find");
    a.dex();
    a.bmi("firm_own_found");
    a.tya();
    a.sec();
    a.adc(abs("firm_keywords").y());
    a.tay();
    a.bne("firm_own_find");
    a.here("firm_own_found");
    a.lda(abs("firm_keywords").y());
    a.sta(abs(var::FIRM_LENGTH));
    a.iny();
    a.sty(abs(var::FIRM_POINTER));
    a.rts();

    // A = the firm key's next character.
    a.here("firm_key_next");
    a.dec(abs(var::FIRM_LENGTH));
    a.jsr("firm_by_language");
    a.beq("firm_own_next");
    a.lda(imm(FIRM_NEXT));
    a.jsr("language_call");
    a.tya();
    a.rts();
    a.here("firm_own_next");
    a.ldx(abs(var::FIRM_POINTER));
    a.inc(abs(var::FIRM_POINTER));
    a.lda(abs("firm_keywords").x());
    a.rts();

    // Z clear if the language expands firm keys itself.
    a.here("firm_by_language");
    a.ldx(abs(var::LANGUAGE));
    a.lda(abs(page2::ROM_TYPES).x());
    a.and(imm(crate::rom::os::rom_type::FIRM_KEYS));
    a.rts();

    // The language entered with reason A and Y, the ROM paged before back
    // after; Y is its answer.
    a.here("language_call");
    a.tax();
    a.lda(zp(os::ROM));
    a.pha();
    a.txa();
    a.pha();
    a.ldx(abs(var::LANGUAGE));
    a.jsr("page_x");
    a.pla();
    a.jsr(SIDEWAYS_BASE);
    a.pla();
    a.jmp("page_a");

    a.later(|a| {
        a.here("firm_keywords");
        for keyword in FIRM_KEYWORDS {
            a.byte(byte(keyword.len()));
            a.text(keyword);
        }
    });
    a.later(|a| {
        a.here("firm_symbols");
        a.bytes(&FIRM_SYMBOLS);
    });
    a.later(|a| {
        a.here("cursor_shifted");
        a.bytes(&CURSOR_SHIFTED);
    });
    a.later(|a| {
        a.here("cursor_controlled");
        a.bytes(&CURSOR_CONTROLLED);
    });
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
    a.error(0xFB, "Bad key");
    a.here("key_in_use");
    a.error(0xFA, "Key in use");
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
