//! The keyboard: SCNKEY's scan of the matrix, KEYLOG's default picking a
//! table by the shift keys held and decoding the key into the buffer, the
//! repeat, and taking a key from the buffer.
//!
//! The four tables give each key's PETSCII code alone, with SHIFT, with the
//! Commodore key and with CTRL, as the original's keyboard produces them.

use super::ws::{page, zp};
use crate::machine::c64::cia::reg;
use crate::machine::c64::io::{self, cia1};
use crate::machine::c64::keyboard::Key;
use crate::machine::c64::vic::reg as vic;
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Acc, Asm, RTS};

/// A key with no code in a table.
const NONE: u8 = 0xFF;
/// SFDX and LSTX with no key held.
pub const NO_KEY: u8 = 64;

/// SHFLAG's bits.
pub mod shift {
    /// Either SHIFT, or SHIFT LOCK.
    pub const SHIFT: u8 = 0x01;
    /// The Commodore key.
    pub const COMMODORE: u8 = 0x02;
    /// CTRL.
    pub const CONTROL: u8 = 0x04;
}

/// The keys the shift flags come from, and the flag each sets.
const SHIFT_KEYS: [(Key, u8); 4] = [
    (Key::LEFT_SHIFT, shift::SHIFT),
    (Key::RIGHT_SHIFT, shift::SHIFT),
    (Key::COMMODORE, shift::COMMODORE),
    (Key::CONTROL, shift::CONTROL),
];

/// The keys that repeat while held unless RPTFLG's bit 7 makes every key
/// repeat or its bit 6 none.
const REPEATING: [Key; 4] = [Key::DELETE, Key::CURSOR_RIGHT, Key::CURSOR_DOWN, Key::SPACE];

/// The STOP key's column, selected while the keyboard is idle and by UDTIM
/// to read STOP.
pub const STOP_COLUMN: u8 = !(1 << Key::RUN_STOP.column());
/// The STOP key alone, read with its column selected: its row pulled low.
pub const STOP_ROW: u8 = !(1 << Key::RUN_STOP.row());

/// The delay before a held key's first repeat and between repeats, in
/// jiffies, as a new key sets them.
pub const DELAY_JIFFIES: u8 = 16;
pub const KOUNT_JIFFIES: u8 = 4;
/// The delay CINT leaves before any key is pressed, as the original's does.
pub const DELAY_AT_RESET: u8 = 10;

/// The character set's bit in the VIC-II's memory register: the lower-case
/// set.
pub const LOWER_CASE: u8 = 0x02;

#[rustfmt::skip]
const UNSHIFTED: [u8; 64] = [
    0x14, 0x0D, 0x1D, 0x88, 0x85, 0x86, 0x87, 0x11,
    0x33, 0x57, 0x41, 0x34, 0x5A, 0x53, 0x45, NONE,
    0x35, 0x52, 0x44, 0x36, 0x43, 0x46, 0x54, 0x58,
    0x37, 0x59, 0x47, 0x38, 0x42, 0x48, 0x55, 0x56,
    0x39, 0x49, 0x4A, 0x30, 0x4D, 0x4B, 0x4F, 0x4E,
    0x2B, 0x50, 0x4C, 0x2D, 0x2E, 0x3A, 0x40, 0x2C,
    0x5C, 0x2A, 0x3B, 0x13, NONE, 0x3D, 0x5E, 0x2F,
    0x31, 0x5F, NONE, 0x32, 0x20, NONE, 0x51, 0x03,
];

#[rustfmt::skip]
const SHIFTED: [u8; 64] = [
    0x94, 0x8D, 0x9D, 0x8C, 0x89, 0x8A, 0x8B, 0x91,
    0x23, 0xD7, 0xC1, 0x24, 0xDA, 0xD3, 0xC5, NONE,
    0x25, 0xD2, 0xC4, 0x26, 0xC3, 0xC6, 0xD4, 0xD8,
    0x27, 0xD9, 0xC7, 0x28, 0xC2, 0xC8, 0xD5, 0xD6,
    0x29, 0xC9, 0xCA, 0x30, 0xCD, 0xCB, 0xCF, 0xCE,
    0xDB, 0xD0, 0xCC, 0xDD, 0x3E, 0x5B, 0xBA, 0x3C,
    0xA9, 0xC0, 0x5D, 0x93, NONE, 0x3D, 0xDE, 0x3F,
    0x21, 0x5F, NONE, 0x22, 0xA0, NONE, 0xD1, 0x83,
];

#[rustfmt::skip]
const COMMODORE: [u8; 64] = [
    0x94, 0x8D, 0x9D, 0x8C, 0x89, 0x8A, 0x8B, 0x91,
    0x96, 0xB3, 0xB0, 0x97, 0xAD, 0xAE, 0xB1, NONE,
    0x98, 0xB2, 0xAC, 0x99, 0xBC, 0xBB, 0xA3, 0xBD,
    0x9A, 0xB7, 0xA5, 0x9B, 0xBF, 0xB4, 0xB8, 0xBE,
    0x29, 0xA2, 0xB5, 0x30, 0xA7, 0xA1, 0xB9, 0xAA,
    0xA6, 0xAF, 0xB6, 0xDC, 0x3E, 0x5B, 0xA4, 0x3C,
    0xA8, 0xDF, 0x5D, 0x93, NONE, 0x3D, 0xDE, 0x3F,
    0x81, 0x5F, NONE, 0x95, 0xA0, NONE, 0xAB, 0x83,
];

#[rustfmt::skip]
const CONTROL: [u8; 64] = [
    NONE, NONE, NONE, NONE, NONE, NONE, NONE, NONE,
    0x1C, 0x17, 0x01, 0x9F, 0x1A, 0x13, 0x05, NONE,
    0x9C, 0x12, 0x04, 0x1E, 0x03, 0x06, 0x14, 0x18,
    0x1F, 0x19, 0x07, 0x9E, 0x02, 0x08, 0x15, 0x16,
    0x12, 0x09, 0x0A, 0x92, 0x0D, 0x0B, 0x0F, 0x0E,
    NONE, 0x10, 0x0C, NONE, NONE, 0x1B, 0x00, NONE,
    0x1C, NONE, 0x1D, NONE, NONE, 0x1F, 0x1E, NONE,
    0x90, 0x06, NONE, 0x05, NONE, NONE, 0x11, NONE,
];

/// The tables by the shift flags' combination, each a table's bytes on from
/// the last: none, SHIFT, the Commodore key, and CTRL's, which takes every
/// combination with CTRL in it.
const TABLES: [&[u8; 64]; 4] = [&UNSHIFTED, &SHIFTED, &COMMODORE, &CONTROL];
/// A table's bytes, a code a key.
const TABLE_BYTES: u8 = 64;

const _: () = assert!(
    shift::SHIFT == 1 && shift::COMMODORE == 2 && shift::CONTROL == 4,
    "SHFLAG's low two bits number the tables and CTRL's is the bit above"
);

pub fn emit(a: &mut Asm) {
    scan(a);
    decode(a);
    take(a);
    // One run, so the tables stay a table's bytes apart.
    a.later(|a| {
        a.here("key_tables");
        for table in TABLES {
            a.bytes(table);
        }
    });
}

/// SCNKEY: every column driven low in turn and its rows read, the highest
/// numbered key held kept in SFDX and the shift keys gathered in SHFLAG,
/// then KEYLOG. With no key held KEYLOG is not called: LSTX and LSTSHF are
/// cleared and the repeat's counters left as they stand.
fn scan(a: &mut Asm) {
    a.here("scnkey");
    let (held, column, settle, row, set, kept, next) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    let stop_column = |a: &mut Asm| {
        a.lda(imm(STOP_COLUMN));
        a.sta(abs(cia1(reg::PRA)));
    };
    a.lda(imm(0));
    a.sta(abs(page::SHFLAG));
    a.ldy(imm(NO_KEY));
    a.sty(z(zp::SFDX));
    a.sta(abs(cia1(reg::PRA)));
    a.ldx(abs(cia1(reg::PRB)));
    a.inx();
    a.bne(held);
    a.sty(z(zp::LSTX));
    a.sta(abs(page::LSTSHF));
    stop_column(a);
    a.rts();
    a.bind(held);
    // A is still 0, the first key's number.
    a.tay();
    a.lda(imm(0xFE));
    a.bind(column);
    a.sta(abs(cia1(reg::PRA)));
    a.pha();
    a.bind(settle);
    a.lda(abs(cia1(reg::PRB)));
    a.cmp(abs(cia1(reg::PRB)));
    a.bne(settle);
    // The match leaves C set: shifted in above the rows, the last bit out.
    a.ror(Acc);
    a.bind(row);
    a.bcs(next);
    // Key Y is held: a shift key's flag, or the key.
    a.pha();
    let mut flag = 0;
    for (key, key_flag) in SHIFT_KEYS {
        if key_flag != flag {
            if key_flag == flag << 1 {
                a.asl(Acc);
            } else {
                a.lda(imm(key_flag));
            }
            flag = key_flag;
        }
        a.cpy(imm(key.code()));
        a.beq(set);
    }
    a.sty(z(zp::SFDX));
    a.bne(kept);
    a.bind(set);
    a.ora(abs(page::SHFLAG));
    a.sta(abs(page::SHFLAG));
    a.bind(kept);
    a.pla();
    a.bind(next);
    a.iny();
    a.lsr(Acc);
    a.bne(row);
    a.pla();
    // The column's 0 moves up a line, out of the top after the last.
    a.sec();
    a.rol(Acc);
    a.bcs(column);
    stop_column(a);
    a.jmp(abs(page::KEYLOG).ind());
}

/// KEYLOG's default: with SHIFT and the Commodore key alone held, the
/// character set changes as they go down unless MODE forbids it and no key
/// is decoded; otherwise the table the shift keys pick decodes the key,
/// held keys repeating.
fn decode(a: &mut Asm) {
    a.here("keylog");
    let (pick, chosen, held, repeat, eligible, count, load) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(abs(page::SHFLAG));
    a.ldx(abs(page::LSTSHF));
    a.sta(abs(page::LSTSHF));
    a.cmp(imm(shift::SHIFT | shift::COMMODORE));
    a.bne(pick);
    a.cpx(imm(shift::SHIFT | shift::COMMODORE));
    a.beq(held);
    a.bit(abs(page::MODE));
    a.bmi(held);
    let memory = io::vic(vic::MEMORY);
    a.lda(abs(memory));
    a.eor(imm(LOWER_CASE));
    a.sta(abs(memory));
    a.bind(held);
    a.lda(z(zp::SFDX));
    a.sta(z(zp::LSTX));
    a.rts();

    // SHFLAG's low bits rotated to the top make the table's offset, CTRL's
    // bit left in C picking CTRL's table whatever else is held.
    a.bind(pick);
    a.lsr(Acc);
    a.ror(Acc);
    a.ror(Acc);
    a.bcc(chosen);
    // C adds the last one.
    a.lda(imm(3 * TABLE_BYTES - 1));
    a.bind(chosen);
    a.adc(imm(lo("key_tables")));
    a.sta(z(zp::KEYTAB));
    a.lda(imm(hi("key_tables")));
    a.adc(imm(0));
    a.sta(z(zp::KEYTAB + 1));
    a.ldy(z(zp::SFDX));
    a.cpy(z(zp::LSTX));
    a.beq(repeat);
    a.lda(imm(DELAY_JIFFIES));
    a.sta(abs(page::DELAY));
    a.lda(imm(KOUNT_JIFFIES));
    a.sta(abs(page::KOUNT));
    a.sty(z(zp::LSTX));
    // No key runs on to return at the repeat's own test.
    a.cpy(imm(NO_KEY));
    a.bne(load);

    a.bind(repeat);
    a.cpy(imm(NO_KEY));
    a.beq(RTS);
    a.bit(abs(page::RPTFLG));
    a.bmi(eligible);
    a.bvs(RTS);
    for key in REPEATING {
        // Key 0 is tested a byte shorter.
        if key.code() == 0 {
            a.tya();
        } else {
            a.cpy(imm(key.code()));
        }
        a.beq(eligible);
    }
    a.rts();
    a.bind(eligible);
    a.lda(abs(page::DELAY));
    a.beq(count);
    a.dec(abs(page::DELAY));
    a.bne(RTS);
    a.bind(count);
    a.dec(abs(page::KOUNT));
    a.bne(RTS);
    a.lda(imm(KOUNT_JIFFIES));
    a.sta(abs(page::KOUNT));
    a.lda(z(zp::NDX));
    a.bne(RTS);
    a.bind(load);
    a.lda(z(zp::KEYTAB).ind_y());
    a.cmp(imm(NONE));
    a.beq(RTS);
    a.ldx(z(zp::NDX));
    a.cpx(abs(page::XMAX));
    a.bcs(RTS);
    a.sta(abs(page::KEYD).x());
    a.inc(z(zp::NDX));
    a.rts();
}

/// The first key from the buffer, which holds one at least, into A, the
/// rest moved up; called with interrupts off, which it turns back on. C
/// clear.
fn take(a: &mut Asm) {
    a.here("take_key");
    let shift = a.local();
    a.ldy(abs(page::KEYD));
    a.ldx(imm(0));
    a.bind(shift);
    a.lda(abs(page::KEYD + 1).x());
    a.sta(abs(page::KEYD).x());
    a.inx();
    a.cpx(z(zp::NDX));
    a.bcc(shift);
    a.dec(z(zp::NDX));
    a.tya();
    a.cli();
    a.clc();
    a.rts();
}
