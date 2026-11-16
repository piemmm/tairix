//! Starting and restarting: the reset and its cartridge check, IOINIT,
//! RAMTAS, RESTOR and VECTOR, the NMI with RESTORE, and the warm start BRK
//! and RUN/STOP-RESTORE share.

use super::keyboard::STOP_COLUMN;
use super::rs232::TXD;
use super::ws::{page, vector, zp};
use super::{fixed, restore_and_return, vector_table};
use crate::machine::c64::cia::{cr, icr, reg};
use crate::machine::c64::io::{cia1, cia2};
use crate::machine::c64::memory::line;
use crate::machine::c64::sid::reg as sid;
use crate::machine::c64::{io, serial, Standard};
use crate::rom::asm::{abs, hi, imm, lo, sym, zp as z, Acc, Asm, RTS};

/// A cartridge's header: its cold start, its warm start, then the signature
/// the reset and RESTORE look for, "CBM80" with the letters' top bits set.
const CARTRIDGE: u16 = 0x8000;
const CARTRIDGE_WARM: u16 = CARTRIDGE + 2;
const SIGNATURE_AT: u16 = CARTRIDGE + 4;
const CARTRIDGE_SIGNATURE: [u8; 5] = [0xC3, 0xC2, 0xCD, 0x38, 0x30];

/// BASIC's cold and warm starts, the vectors its ROM opens with.
const BASIC_COLD: u16 = 0xA000;
const BASIC_WARM: u16 = 0xA002;

/// The memory test starts at the bottom of BASIC's memory, and leaves the
/// screen at page 4.
const BOTTOM: u16 = 0x0800;
const SCREEN_PAGE: u8 = 0x04;
/// The memory test's first pattern; the second, its shift, has every bit
/// the other way.
const PATTERN: u8 = 0x55;

const _: () = assert!(
    PATTERN << 1 == !PATTERN,
    "the patterns are each other's inverse"
);
const _: () = assert!(
    BOTTOM.to_le_bytes()[0] == 0,
    "RAMTAS's clear leaves MEMSTR's low byte"
);

/// The 6510 port as IOINIT leaves it: the ROMs and I/O in, the cassette's
/// motor off and its write line low, those lines and the three banking
/// lines driven.
const PORT_DATA: u8 = line::LORAM | line::HIRAM | line::CHAREN | line::CASSETTE_MOTOR;
const PORT_DIRECTION: u8 = line::BANKING | line::CASSETTE_WRITE | line::CASSETTE_MOTOR;

/// CIA2's port A as IOINIT leaves it: the VIC-II's bank 0, RS-232's output
/// high, and the serial bus's CLOCK held low, its other lines free.
const CIA2_PORT_A: u8 = serial::BANK | TXD | serial::CLOCK_OUT;
/// CIA2's port A lines driven: all but the serial bus's two inputs.
const CIA2_DIRECTION_A: u8 = !(serial::CLOCK_IN | serial::DATA_IN);

/// Every interrupt cause's mask bit, written clear.
const MASK_ALL: u8 = !icr::SET;

/// Timer A's latch for the jiffy interrupt: a sixtieth of a second.
const fn jiffy_latch(standard: Standard) -> [u8; 2] {
    let cycles = (standard.clock() + 30) / 60;
    assert!(cycles < 0x1_0000, "a sixtieth of a second fits the timer");
    let bytes = cycles.to_le_bytes();
    [bytes[0], bytes[1]]
}

const PAL_JIFFY: [u8; 2] = jiffy_latch(Standard::Pal);
const NTSC_JIFFY: [u8; 2] = jiffy_latch(Standard::Ntsc);

const _: () = assert!(
    fixed::NMI_ENTRY + 4 == fixed::NMI,
    "the NMI's entry runs on into NMINV's default"
);

/// The NMI's entry, NMINV's way to its handler, and the handler's end.
pub fn pin(a: &mut Asm) {
    a.pin(fixed::NMI_ENTRY, |a| {
        a.sei();
        a.jmp(abs(vector::NMINV).ind());
        a.jmp("nmi_handler");
    });
    a.pin(fixed::NMI_EXIT, restore_and_return);
}

pub fn emit(a: &mut Asm) {
    reset(a);
    ioinit(a);
    ramtas(a);
    restor(a);
    vector_table(a);
    nmi(a);
    warm_start(a);
}

fn reset(a: &mut Asm) {
    a.here("reset");
    a.ldx(imm(0xFF));
    a.sei();
    a.txs();
    a.cld();
    a.jsr("find_cartridge");
    let basic = a.local();
    a.bne(basic);
    a.jmp(abs(CARTRIDGE).ind());
    a.bind(basic);
    a.jsr("ioinit");
    a.jsr("ramtas");
    a.jsr("restor");
    a.jsr("cint");
    a.cli();
    a.jmp(abs(BASIC_COLD).ind());

    // Z set where a cartridge's signature is at $8004.
    a.here("find_cartridge");
    a.ldx(imm(5));
    let compare = a.here_local();
    a.lda(abs(SIGNATURE_AT - 1).x());
    a.cmp(abs(sym("cartridge_signature") + -1).x());
    a.bne(RTS);
    a.dex();
    a.bne(compare);
    a.rts();
    a.later(|a| {
        a.here("cartridge_signature");
        a.bytes(&CARTRIDGE_SIGNATURE);
    });
}

/// IOINIT: the CIAs' interrupts off and their timers stopped, the keyboard's
/// columns driven, the serial bus idle, the SID silent, the 6510 port set,
/// then the jiffy interrupt started.
fn ioinit(a: &mut Asm) {
    a.here("ioinit");
    a.lda(imm(MASK_ALL));
    a.sta(abs(cia1(reg::ICR)));
    a.sta(abs(cia2(reg::ICR)));
    a.lda(imm(STOP_COLUMN));
    a.sta(abs(cia1(reg::PRA)));
    a.lda(imm(cr::ONE_SHOT));
    for register in [reg::CRA, reg::CRB] {
        a.sta(abs(cia1(register)));
        a.sta(abs(cia2(register)));
    }
    a.ldx(imm(0));
    a.stx(abs(cia1(reg::DDRB)));
    a.stx(abs(cia2(reg::DDRB)));
    a.stx(abs(io::SID + u16::from(sid::MODE_VOLUME)));
    a.dex();
    a.stx(abs(cia1(reg::DDRA)));
    a.lda(imm(CIA2_PORT_A));
    a.sta(abs(cia2(reg::PRA)));
    a.lda(imm(CIA2_DIRECTION_A));
    a.sta(abs(cia2(reg::DDRA)));
    a.lda(imm(PORT_DATA));
    a.sta(z(1));
    a.lda(imm(PORT_DIRECTION));
    a.sta(z(0));
    a.fall_into("start_jiffies");

    // Timer A running continuously at the standard's sixtieth of a second,
    // its underflow CIA1's interrupt. The time of day keeps the rate the
    // control register had.
    a.here("start_jiffies");
    let set = a.local();
    a.lda(imm(PAL_JIFFY[0]));
    a.ldx(imm(PAL_JIFFY[1]));
    a.ldy(abs(page::TVSFLG));
    a.bne(set);
    a.lda(imm(NTSC_JIFFY[0]));
    a.ldx(imm(NTSC_JIFFY[1]));
    a.bind(set);
    a.sta(abs(cia1(reg::TA_LO)));
    a.stx(abs(cia1(reg::TA_HI)));
    a.lda(imm(icr::SET | icr::TA));
    a.sta(abs(cia1(reg::ICR)));
    a.lda(abs(cia1(reg::CRA)));
    a.and(imm(cr::TOD_50HZ));
    a.ora(imm(cr::FORCE_LOAD | cr::START));
    a.sta(abs(cia1(reg::CRA)));
    a.rts();
}

/// RAMTAS: pages 0, 2 and 3 cleared from `$0002`, the tape buffer's pointer
/// set, and the top of memory found by testing a byte of each page from the
/// bottom up, each byte given back what it held.
fn ramtas(a: &mut Asm) {
    a.here("ramtas");
    a.lda(imm(0));
    a.tay();
    let clear = a.here_local();
    a.sta(abs(0x0002).y());
    a.sta(abs(0x0200).y());
    a.sta(abs(0x0300).y());
    a.iny();
    a.bne(clear);
    // What the clear leaves 0 needs no store: TEMP's low byte and the low
    // bytes of MEMSTR and MEMSIZ. Y stays 0.
    a.lda(imm(lo(page::TBUFFR)));
    a.sta(z(zp::TAPE1));
    a.lda(imm(hi(page::TBUFFR)));
    a.sta(z(zp::TAPE1 + 1));
    a.lda(imm(hi(BOTTOM)));
    a.sta(z(zp::TEMP + 1));
    a.sta(abs(page::MEMSTR + 1));
    let (test, top) = (a.local(), a.local());
    a.bind(test);
    a.lda(z(zp::TEMP).ind_y());
    a.tax();
    a.lda(imm(PATTERN));
    a.sta(z(zp::TEMP).ind_y());
    a.cmp(z(zp::TEMP).ind_y());
    a.bne(top);
    a.asl(Acc);
    a.sta(z(zp::TEMP).ind_y());
    a.cmp(z(zp::TEMP).ind_y());
    a.bne(top);
    a.txa();
    a.sta(z(zp::TEMP).ind_y());
    a.inc(z(zp::TEMP + 1));
    a.bne(test);
    a.bind(top);
    a.txa();
    a.sta(z(zp::TEMP).ind_y());
    a.lda(z(zp::TEMP + 1));
    a.sta(abs(page::MEMSIZ + 1));
    a.lda(imm(SCREEN_PAGE));
    a.sta(abs(page::HIBASE));
    a.rts();
}

/// RESTOR, the defaults copied into the RAM vectors, and VECTOR: with C
/// set the vectors copied to the table at (X, Y), with C clear the table
/// copied into them. Interrupts are held off so none meets a vector half
/// changed.
fn restor(a: &mut Asm) {
    a.here("restor");
    a.ldx(imm(lo("vector_defaults")));
    a.ldy(imm(hi("vector_defaults")));
    a.clc();
    a.here("vector");
    a.stx(z(zp::MEMUSS));
    a.sty(z(zp::MEMUSS + 1));
    a.php();
    a.sei();
    a.ldy(imm(vector::BYTES - 1));
    let (copy, set, next) = (a.here_local(), a.local(), a.local());
    a.bcc(set);
    a.lda(abs(vector::CINV).y());
    a.sta(z(zp::MEMUSS).ind_y());
    a.bcs(next);
    a.bind(set);
    a.lda(z(zp::MEMUSS).ind_y());
    a.sta(abs(vector::CINV).y());
    a.bind(next);
    a.dey();
    a.bpl(copy);
    a.plp();
    a.rts();
}

/// The NMI: CIA2's causes are RS-232's; otherwise it is RESTORE, which
/// starts a cartridge's warm start, or with STOP held (as the STOP routine
/// through its vector finds it) the warm start.
fn nmi(a: &mut Asm) {
    a.here("nmi_handler");
    a.pha();
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    let (restore, cartridge) = (a.local(), a.local());
    a.lda(abs(cia2(reg::ICR)));
    a.bpl(restore);
    a.jmp("rs232_nmi");
    a.bind(restore);
    a.jsr("find_cartridge");
    a.beq(cartridge);
    a.jsr("scan_stop");
    a.jsr("STOP");
    // warm_start follows.
    a.beq("warm_start");
    a.jmp(abs(fixed::NMI_EXIT));
    a.bind(cartridge);
    a.jmp(abs(CARTRIDGE_WARM).ind());
}

/// BRK's and RUN/STOP-RESTORE's restart: the vectors, the I/O and the
/// screen set up again, then BASIC's warm start.
fn warm_start(a: &mut Asm) {
    a.here("warm_start");
    a.jsr("restor");
    a.jsr("ioinit");
    a.jsr("cint");
    a.jmp(abs(BASIC_WARM).ind());
}
