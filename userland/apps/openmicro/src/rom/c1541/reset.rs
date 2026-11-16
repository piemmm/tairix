//! Reset and the idle loop: page zero and page two cleared and every
//! channel left with no file, VIA1 on the
//! bus with ATN's edge latched, VIA2's mechanism outputs set and the motor
//! stopped, the device number read from the jumpers, the power-on message
//! set, then ATN waited for.

use super::errors::number;
use super::ws::{page, zp, STACK_TOP};
use super::{bus, controller};
use crate::machine::c64::drive::{bus as lines, mechanism, via1, via2};
use crate::machine::via::{irq, reg, ACR_LATCH_A, ACR_T1_CONTINUOUS};
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Acc, Asm};

/// VIA1's PCR: CA1 sees ATN asserted, its input rising.
const ATN_RISING: u8 = 0x01;
/// Every interrupt disabled, the IER's set bit clear.
const DISABLE_ALL: u8 = !irq::ANY;
/// VIA2's port B lines the DOS drives.
const MECHANISM_OUTPUTS: u8 =
    mechanism::PHASE | mechanism::MOTOR | mechanism::LED | mechanism::ZONE;

pub fn emit(a: &mut Asm) {
    a.here("reset");
    let clear = a.local();
    a.sei();
    a.cld();
    a.ldx(imm(STACK_TOP));
    a.txs();
    a.lda(imm(lines::OUTPUTS));
    a.sta(abs(via1(reg::DDRB)));
    a.lda(imm(MECHANISM_OUTPUTS));
    a.sta(abs(via2(reg::DDRB)));
    a.lda(imm(mechanism::LED));
    a.sta(abs(via2(reg::ORB)));
    a.lda(imm(DISABLE_ALL));
    a.sta(abs(via1(reg::IER)));
    a.sta(abs(via2(reg::IER)));
    a.lda(imm(ATN_RISING));
    a.sta(abs(via1(reg::PCR)));
    a.lda(imm(0));
    a.sta(abs(via1(reg::ORB)));
    a.tax();
    a.bind(clear);
    a.sta(z(0).x());
    a.sta(abs(page::COMMAND).x());
    a.inx();
    a.bne(clear);
    a.jsr("drop_files");
    a.lda(abs(via1(reg::ORB)));
    a.and(imm(lines::JUMPERS));
    for _ in 0..lines::JUMPERS.trailing_zeros() {
        a.lsr(Acc);
    }
    a.ora(imm(DEVICE_BASE));
    a.sta(z(zp::DEVICE));
    a.lda(imm(number::POWER_ON));
    a.ldx(imm(0));
    a.ldy(imm(0));
    a.jsr("set_error");
    // VIA2: reading with BYTE READY on SO, the data latched, timer A
    // ticking the controller, and the phase where the head rests.
    a.lda(imm(controller::READING));
    a.sta(abs(via2(reg::PCR)));
    a.lda(imm(ACR_LATCH_A | ACR_T1_CONTINUOUS));
    a.sta(abs(via2(reg::ACR)));
    a.lda(imm(0));
    a.sta(abs(via2(reg::DDRA)));
    a.lda(imm(lo(controller::TICK)));
    a.sta(abs(via2(reg::T1C_L)));
    a.lda(imm(hi(controller::TICK)));
    a.sta(abs(via2(reg::T1C_H)));
    a.lda(imm(irq::ANY | irq::T1));
    a.sta(abs(via2(reg::IER)));
    // Edges latched before the PCRs were set are not ATN.
    a.lda(imm(!irq::ANY));
    a.sta(abs(via1(reg::IFR)));
    a.sta(abs(via2(reg::IFR)));
    a.lda(imm(controller::HOME_HALF_TRACK));
    a.sta(z(zp::HALF_TRACK));
    a.and(imm(mechanism::PHASE));
    a.sta(abs(via2(reg::ORB)));

    // ATN's edge is latched whatever the drive was doing, so a command is
    // never cut short; the service starts from here. ATN held on the way
    // in came while the drive restarted, its edge cleared with the rest.
    // The controller runs meanwhile, whatever the bus left the interrupt
    // mask.
    a.here("idle");
    let (watch, attention) = (a.local(), a.local());
    a.cli();
    a.lda(abs(bus::PORT));
    a.bmi(attention);
    a.bind(watch);
    a.lda(abs(bus::FLAGS));
    a.and(imm(irq::CA1));
    a.beq(watch);
    a.bind(attention);
    a.jmp("atn_service");
}

/// The lowest device number, the jumpers' value added.
const DEVICE_BASE: u8 = 8;
