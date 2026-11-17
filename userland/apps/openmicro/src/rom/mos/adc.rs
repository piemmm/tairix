//! The ADC: the uPD7002 converting the channels OSBYTE 16 selects in turn,
//! from the highest down, each result kept where ADVAL reads it.

use super::svia;
use super::ws::{page2, var};
use crate::machine::bbc::adc::{self, register};
use crate::machine::bbc::sheila;
use crate::machine::via::{irq, reg};
use crate::rom::asm::{abs, imm, Asm, RTS};
use crate::rom::os::event;

/// OSBYTE &BE's setting for 8-bit conversions; any other is 10-bit.
const EIGHT_BIT: u8 = 8;

const STATUS: u16 = sheila::ADC + register::STATUS as u16;
const HIGH: u16 = sheila::ADC + register::HIGH as u16;
const LOW: u16 = sheila::ADC + register::LOW as u16;

pub fn emit(a: &mut Asm) {
    a.whole(conversions);
}

fn conversions(a: &mut Asm) {
    // From the highest channel OSBYTE 16 selects; with none, those below the
    // one converting still finish, as MOS 1.20's do.
    a.here("adc_init");
    a.here("adc_restart");
    a.lda(abs(var::ADC_MAXIMUM));
    a.beq(RTS);
    a.sta(abs(var::ADC_CHANNEL));
    // The channel less one and the resolution, unmasked, as MOS 1.20 writes
    // them for channel 0 too: in the four bits the chip reads, EOR adds 8.
    a.here("adc_start");
    a.ldx(abs(var::ADC_CHANNEL));
    a.dex();
    a.txa();
    a.ldx(abs(var::ADC_TYPE));
    a.cpx(imm(EIGHT_BIT));
    a.beq("adc_start_resolution");
    a.eor(imm(adc::TEN_BIT));
    a.here("adc_start_resolution");
    a.sta(abs(STATUS));
    a.rts();

    // OSBYTE 17: channel X at once, at most the last, ADVAL(0)'s channel
    // clear until it is done; channel 0's result is kept nowhere.
    a.here("adc_force");
    a.cpx(imm(page2::ADC_CHANNELS + 1));
    let channel = a.local();
    a.bcc(channel);
    a.ldx(imm(page2::ADC_CHANNELS));
    a.bind(channel);
    a.stx(abs(var::ADC_CHANNEL));
    a.lda(imm(0));
    a.sta(abs(page2::ADC_LAST));
    a.beq("adc_start");

    // End of conversion: the result kept, the event, and the next channel
    // down, the highest again after channel 1. Writing port B clears CB1's
    // flag, so the tick looks at the chip too, and whichever comes first
    // takes the result.
    a.here("adc_interrupt");
    a.lda(imm(irq::CB1));
    a.sta(abs(svia(reg::IFR)));
    a.here("adc_poll");
    a.lda(abs(var::ADC_CHANNEL));
    a.beq(RTS);
    a.lda(abs(STATUS));
    a.bmi(RTS);
    a.and(imm(adc::CHANNEL));
    a.tax();
    a.lda(abs(LOW));
    a.sta(abs(page2::ADC_LOW).x());
    a.lda(abs(HIGH));
    a.sta(abs(page2::ADC_HIGH).x());
    a.inx();
    a.stx(abs(page2::ADC_LAST));
    a.txa();
    a.ldy(imm(event::CONVERTED));
    a.jsr("oseven");
    a.ldx(abs(var::ADC_CHANNEL));
    a.dex();
    a.bne("adc_next");
    a.ldx(abs(var::ADC_MAXIMUM));
    a.here("adc_next");
    a.stx(abs(var::ADC_CHANNEL));
    a.txa();
    a.bne("adc_start");
    a.rts();
}
