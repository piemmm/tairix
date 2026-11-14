//! Interrupts: the IRQ and BRK entry; IRQ1V's handler, on the Model B for
//! the 6850, the two VIAs and their causes, and on the Electron for the
//! ULA's two ten-millisecond ticks, RTC and display end; the 100 Hz tick,
//! VSYNC's flashing, and the hardware setup reset performs.

use super::ws::{os, page2, var, vector};
use super::{latch, svia, uvia};
use crate::machine::bbc::latch as machine_latch;
use crate::machine::bbc::sheila;
use crate::machine::bbc::via::{irq, reg};
use crate::machine::electron::{irq as ula_irq, paging, sheila as ula};
use crate::machine::Model;
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Acc, Asm, RTS};
use crate::rom::os::{event, service};

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

pub fn emit(a: &mut Asm, model: Model) {
    entry(a);
    match model {
        Model::ModelB => handler(a),
        Model::Electron => electron_handler(a),
    }
    tick(a, model);
    vsync(a, model);
    match model {
        Model::ModelB => hardware(a),
        Model::Electron => electron_hardware(a),
    }
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
    a.lda(imm(service::INTERRUPT));
    a.jsr("service_call");
    a.jmp("irq_done");
}

/// IRQ1V on the Electron: RTC is a tick, and display end the VSYNC's work
/// and the other tick; anything else is IRQ2V's.
fn electron_handler(a: &mut Asm) {
    a.here("irq1_default");
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.lda(abs(ula::STATUS));
    a.and(abs(var::ULA_ENABLES));
    a.tax();
    a.and(imm(ula_irq::RTC));
    a.beq("irq_not_rtc");
    a.lda(imm(paging::CLEAR_RTC));
    a.jsr("ula_clear");
    a.jsr("tick_work");
    a.jmp("irq_done");
    a.here("irq_not_rtc");
    a.txa();
    a.and(imm(ula_irq::DISPLAY_END));
    a.beq("irq_unrecognised");
    a.lda(imm(paging::CLEAR_DISPLAY_END));
    a.jsr("ula_clear");
    a.jsr("irq_vsync");
    a.jsr("tick_work");
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

    // IRQ2V: nothing else is the OS's.
    a.here("irq2_default");
    a.lda(zp(os::IRQ_A));
    a.rti();

    // Clear the ULA's conditions A names, keeping the ROM paged.
    a.here("ula_clear");
    a.ora(zp(os::ROM));
    a.sta(abs(ula::PAGING));
    a.rts();
}

/// The 100 Hz tick: TIME in its two copies, the interval timer, INKEY's
/// countdown, the printer, the Model B's ADC or the Electron's paged ROMs'
/// poll, sound and the keyboard.
fn tick(a: &mut Asm, model: Model) {
    if model == Model::ModelB {
        a.here("irq_tick");
        a.lda(imm(irq::T1));
        a.sta(abs(svia(reg::IFR)));
    }
    a.here("tick_work");
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
    a.ldy(imm(event::INTERVAL));
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
    match model {
        Model::ModelB => a.jsr("adc_poll"),
        Model::Electron => a.jsr("poll_roms"),
    }
    a.jsr("sound_tick");
    match model {
        Model::ModelB => {
            // KEYV, C and V set, while a key is held or the keyboard is to
            // rearm.
            a.lda(zp(os::KEY_LAST));
            a.ora(zp(os::KEY_FIRST));
            a.ora(abs(var::KEYBOARD_SEMAPHORE));
            a.beq(RTS);
            a.sec();
            a.bcs("irq_keyv");
            // A key pressed: KEYV with C clear and V set.
            a.here("irq_keyboard");
            a.clc();
        }
        Model::Electron => {
            // KEYV, C and V set, while the keyboard is scanned and a key is
            // held or was: no key interrupts, so each tick looks.
            a.lda(abs(var::KEYBOARD_SEMAPHORE));
            a.beq(RTS);
            a.lda(zp(os::KEY_LAST));
            a.ora(zp(os::KEY_FIRST));
            a.bne("irq_keyv_scan");
            a.jsr("keyboard_any");
            a.beq(RTS);
            a.here("irq_keyv_scan");
            a.sec();
        }
    }
    a.here("irq_keyv");
    a.bit(abs("rdch_v_set"));
    a.jmp(abs(vector::KEY).ind());

    if model == Model::Electron {
        // Service call &15, while a paged ROM has asked to be polled, Y its
        // count.
        a.here("poll_roms");
        a.ldy(abs(var::POLL_SEMAPHORE));
        a.beq(RTS);
        a.lda(imm(service::POLL));
        a.jmp("service_call");
    }
}

/// VSYNC, the Electron's display end: the cassette's timeout, the flashing
/// colours — the Model B's ULA's flash bit, and on the Electron its palette
/// rewritten — the Electron's cursor, and the event.
fn vsync(a: &mut Asm, model: Model) {
    a.here("irq_vsync");
    if model == Model::ModelB {
        a.lda(imm(irq::CA1));
        a.sta(abs(svia(reg::IFR)));
    }
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
    match model {
        Model::ModelB => a.sta(abs(sheila::VIDEO_CONTROL)),
        Model::Electron => {
            a.jsr("palette_write");
            a.lda(abs(var::ULA_CONTROL));
        }
    }
    a.here("vsync_flash_duration");
    a.and(imm(FLASH));
    a.eor(imm(FLASH));
    a.tay();
    a.lda(abs(var::FLASH_MARK).y());
    a.sta(abs(var::FLASH_COUNT));
    a.here("vsync_flash_done");
    if model == Model::Electron {
        a.jsr("cursor_flash");
    }
    a.ldy(imm(event::VSYNC));
    a.jmp("oseven");
}

/// Reset's setup of the ULA: its enables from their copy, every condition
/// cleared, then `&FE07` from its own.
fn electron_hardware(a: &mut Asm) {
    a.here("hardware_init");
    a.lda(abs(var::ULA_ENABLES));
    a.sta(abs(ula::STATUS));
    a.lda(imm(paging::CLEAR_DISPLAY_END
        | paging::CLEAR_RTC
        | paging::CLEAR_HIGH_TONE));
    a.jsr("ula_clear");
    a.jmp("keyboard_leds");
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
