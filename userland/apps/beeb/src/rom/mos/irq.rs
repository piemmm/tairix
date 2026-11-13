//! Interrupts: the IRQ and BRK entry, IRQ1V's handler for the 6850, the
//! two VIAs and their causes, the 100 Hz tick, VSYNC's flashing, and the
//! hardware setup reset performs.

use super::ws::{os, page2, var, vector};
use super::{latch, svia, uvia};
use crate::machine::latch as machine_latch;
use crate::machine::sheila;
use crate::machine::via::{irq, reg};
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Acc, Asm, RTS};

/// The 6850's status register's IRQ bit.
const ACIA_IRQ: u8 = 0x80;
/// The ULA's flash bit.
const FLASH: u8 = 0x01;
/// Timer 1's latch: ten thousand 1 MHz ticks, a centisecond, between
/// interrupts.
const TICK_LATCH: u16 = 9998;
/// System VIA settings: CA1 and CB1 on falling edges, CA2 rising; timer 1
/// free-running, timer 2 counting PB6; VSYNC, both timers, the ADC and a key
/// pressed interrupting.
const SYSTEM_PCR: u8 = 0x04;
const SYSTEM_ACR: u8 = 0x60;
const SYSTEM_IER: u8 = irq::ANY | irq::CA1 | irq::CA2 | irq::CB1 | irq::T2 | irq::T1;
/// User VIA: the printer's acknowledge on CA1's falling edge, its strobe CA2
/// held high.
const USER_PCR: u8 = 0x0E;
/// The startup links: keys 2 to 9 in row zero.
const FIRST_LINK: u8 = 0x02;
const LAST_LINK: u8 = 0x09;
/// Events.
const EVENT_VSYNC: u8 = 4;
const EVENT_INTERVAL: u8 = 5;
pub fn emit(a: &mut Asm) {
    entry(a);
    handler(a);
    tick(a);
    vsync(a);
    hardware(a);
}

fn entry(a: &mut Asm) {
    a.here("irq_entry");
    a.sta(zp(os::IRQ_A));
    a.pla();
    a.pha();
    a.and(imm(0x10));
    a.beq("irq_vectored");
    a.jmp("brk_entry");
    a.here("irq_vectored");
    a.jmp(abs(vector::IRQ1).ind());
}

/// IRQ1V: the 6850, then the System VIA's causes, then the User VIA's, the
/// rest to IRQ2V.
fn handler(a: &mut Asm) {
    a.here("irq1_default");
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.lda(abs(sheila::ACIA_CONTROL));
    a.and(abs(var::ACIA_MASK));
    a.and(imm(ACIA_IRQ));
    a.beq("irq_not_acia");
    a.jsr("acia_interrupt");
    a.jmp("irq_done");
    a.here("irq_not_acia");
    a.lda(abs(svia(reg::IFR)));
    a.and(abs(svia(reg::IER)));
    a.and(abs(var::SYSTEM_VIA_MASK));
    a.and(imm(!irq::ANY));
    a.bne("irq_system_dispatch");
    a.here("irq_not_system");
    a.lda(abs(uvia(reg::IFR)));
    a.and(abs(uvia(reg::IER)));
    a.and(abs(var::USER_VIA_MASK));
    a.and(imm(irq::CA1));
    a.beq("irq_unrecognised");
    a.jsr("printer_interrupt");
    a.here("irq_done");
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.lda(zp(os::IRQ_A));
    a.rti();
    a.here("irq_unrecognised");
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.lda(zp(os::IRQ_A));
    a.jmp(abs(vector::IRQ2).ind());

    // The System VIA's causes in priority order, A their pending bits: the
    // first pending one's routine returns to irq_done.
    a.here("irq_system_dispatch");
    a.tax();
    a.lda(imm(hi(sym("irq_done") + -1)));
    a.pha();
    a.lda(imm(lo(sym("irq_done") + -1)));
    a.pha();
    for (cause, routine) in [
        (irq::CA1, "irq_vsync"),
        (irq::T1, "irq_tick"),
        (irq::CB1, "adc_interrupt"),
        (irq::T2, "irq_timer_2"),
        (irq::CA2, "irq_keyboard"),
    ] {
        let next = a.local();
        a.txa();
        a.and(imm(cause));
        a.beq(next);
        a.jmp(routine);
        a.bind(next);
    }
    a.pla();
    a.pla();
    a.jmp("irq_not_system");

    // Timer 2 counts nothing with no speech fitted; its flag is cleared.
    a.here("irq_timer_2");
    a.lda(imm(irq::T2));
    a.sta(abs(svia(reg::IFR)));
    a.rts();

    // IRQ2V: offered to the ROMs, then returned from.
    a.here("irq2_default");
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.lda(imm(crate::rom::os::service::INTERRUPT));
    a.jsr("service_call");
    a.jmp("irq_done");
}

/// The 100 Hz tick: TIME in its two copies, the interval timer, INKEY's
/// countdown, sound and the keyboard.
fn tick(a: &mut Asm) {
    a.here("irq_tick");
    a.lda(imm(irq::T1));
    a.sta(abs(svia(reg::IFR)));
    // The copy not current becomes the current one plus one, from its
    // least significant byte; the count is DEC'd to keep the carry.
    a.ldx(abs(var::TIME_SWITCH));
    a.txa();
    a.eor(imm(0x0F));
    a.tay();
    a.lda(imm(5));
    a.sta(zp(os::SCRATCH));
    a.sec();
    a.here("tick_time");
    a.lda(abs(page2::CLOCKS - 1).x());
    a.adc(imm(0));
    a.sta(abs(page2::CLOCKS - 1).y());
    a.dex();
    a.dey();
    a.dec(zp(os::SCRATCH));
    a.bne("tick_time");
    a.lda(abs(var::TIME_SWITCH));
    a.eor(imm(0x0F));
    a.sta(abs(var::TIME_SWITCH));
    // The interval timer counts up; crossing zero is an event.
    a.ldx(imm(4));
    a.sec();
    a.here("tick_interval");
    a.lda(abs(page2::INTERVAL).x());
    a.adc(imm(0));
    a.sta(abs(page2::INTERVAL).x());
    a.dex();
    a.bpl("tick_interval");
    a.bcc("tick_no_interval_event");
    a.ldy(imm(EVENT_INTERVAL));
    a.jsr("oseven");
    a.here("tick_no_interval_event");
    // INKEY's countdown.
    a.lda(abs(page2::INKEY_COUNT));
    a.ora(abs(page2::INKEY_COUNT + 1));
    a.beq("tick_inkey_done");
    a.lda(abs(page2::INKEY_COUNT));
    a.bne("tick_inkey_low");
    a.dec(abs(page2::INKEY_COUNT + 1));
    a.here("tick_inkey_low");
    a.dec(abs(page2::INKEY_COUNT));
    a.here("tick_inkey_done");
    a.jsr("printer_poll");
    a.jsr("adc_poll");
    a.jsr("sound_tick");
    // KEYV, C and V set, while a key is held or the keyboard is to rearm.
    a.lda(zp(os::KEY_LAST));
    a.ora(zp(os::KEY_FIRST));
    a.ora(abs(var::KEYBOARD_SEMAPHORE));
    a.beq(RTS);
    a.sec();
    a.bcs("irq_keyv");
    // A key pressed: KEYV with C clear and V set.
    a.here("irq_keyboard");
    a.clc();
    a.here("irq_keyv");
    a.bit(abs("rdch_v_set"));
    a.jmp(abs(vector::KEY).ind());
}

/// VSYNC: the cassette's timeout, the flashing colours and the event.
fn vsync(a: &mut Asm) {
    a.here("irq_vsync");
    a.lda(imm(irq::CA1));
    a.sta(abs(svia(reg::IFR)));
    a.dec(abs(var::CFS_TIMEOUT));
    // Each state lasts its duration, *FX9's for the first colours and
    // *FX10's for the second; a duration of zero holds the state until it
    // is given one.
    a.lda(abs(var::FLASH_COUNT));
    a.bne("vsync_flash_counting");
    a.lda(abs(var::ULA_CONTROL));
    a.jmp("vsync_flash_duration");
    a.here("vsync_flash_counting");
    a.dec(abs(var::FLASH_COUNT));
    a.bne("vsync_flash_done");
    a.lda(abs(var::ULA_CONTROL));
    a.eor(imm(FLASH));
    a.sta(abs(var::ULA_CONTROL));
    a.sta(abs(sheila::VIDEO_CONTROL));
    a.here("vsync_flash_duration");
    a.and(imm(FLASH));
    a.eor(imm(FLASH));
    a.tay();
    a.lda(abs(var::FLASH_MARK).y());
    a.sta(abs(var::FLASH_COUNT));
    a.here("vsync_flash_done");
    a.ldy(imm(EVENT_VSYNC));
    a.jmp("oseven");
}

/// Reset's setup of the VIAs, the 6850, the serial ULA and the ADC, and
/// the startup links read into OSBYTE &FF's variable.
fn hardware(a: &mut Asm) {
    a.here("hardware_init");
    a.lda(imm(!irq::ANY));
    a.sta(abs(svia(reg::IER)));
    a.sta(abs(uvia(reg::IER)));
    a.sta(abs(svia(reg::IFR)));
    a.sta(abs(uvia(reg::IFR)));
    a.lda(imm(0x0F));
    a.sta(abs(svia(reg::DDRB)));
    // Latch bits 0–3 high: sound and speech idle, the keyboard autoscanning.
    a.ldx(imm(const { latch(machine_latch::SOUND_WE, true) }));
    a.here("hardware_latch");
    a.stx(abs(svia(reg::ORB)));
    a.inx();
    a.cpx(imm(
        const { latch(machine_latch::KEYBOARD_AUTOSCAN, true) + 1 },
    ));
    a.bne("hardware_latch");
    a.lda(imm(SYSTEM_PCR));
    a.sta(abs(svia(reg::PCR)));
    a.lda(imm(SYSTEM_ACR));
    a.sta(abs(svia(reg::ACR)));
    let [low, high] = TICK_LATCH.to_le_bytes();
    a.lda(imm(low));
    a.sta(abs(svia(reg::T1L_L)));
    a.lda(imm(high));
    a.sta(abs(svia(reg::T1L_H)));
    a.sta(abs(svia(reg::T1C_H)));
    a.lda(imm(SYSTEM_IER));
    a.sta(abs(svia(reg::IER)));
    a.lda(imm(USER_PCR));
    a.sta(abs(uvia(reg::PCR)));
    a.lda(imm(0xFF));
    a.sta(abs(uvia(reg::DDRA)));
    a.jsr("serial_init");
    a.jsr("adc_init");
    a.jmp("keyboard_leds");

    a.here("read_links");
    a.lda(imm(0));
    a.sta(abs(var::STARTUP_OPTIONS));
    a.ldx(imm(FIRST_LINK));
    a.here("read_links_next");
    a.jsr("key_down");
    a.eor(imm(super::keyboard::KEY_DOWN));
    a.asl(Acc);
    a.rol(abs(var::STARTUP_OPTIONS));
    a.inx();
    a.cpx(imm(LAST_LINK + 1));
    a.bne("read_links_next");
    a.rts();
}
