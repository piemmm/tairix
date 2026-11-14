//! The `µPD7002` analogue-to-digital converter: four channels, the analogue
//! port's joysticks among them, converted one at a time to twelve bits.
//!
//! A write to the control register starts a conversion of the selected
//! channel; it completes 4 ms later at 8-bit resolution, 10 ms later at
//! 10-bit, when the end-of-conversion line (the System VIA's CB1) falls.

use crate::machine::ANALOGUE_CHANNELS;

/// Cycles an 8-bit conversion takes.
const EIGHT_BIT_CYCLES: u64 = 8_000;
/// Cycles a 10-bit conversion takes.
const TEN_BIT_CYCLES: u64 = 20_000;

/// Control bits: the channel converted.
pub const CHANNEL: u8 = 0x03;
const FLAG: u8 = 0x04;
/// Control bit: a 10-bit conversion rather than an 8-bit one.
pub const TEN_BIT: u8 = 0x08;

/// The registers, from the chip's base address.
pub mod register {
    /// Status on reading; a write starts a conversion.
    pub const STATUS: u8 = 0;
    /// The result's high byte.
    pub const HIGH: u8 = 1;
    /// The result's low byte.
    pub const LOW: u8 = 2;
}

/// What a conversion keeps of its input at each resolution.
const TEN_BIT_PRECISION: u16 = 0xFFC0;
const EIGHT_BIT_PRECISION: u16 = 0xFF00;

/// Status bits.
const RESULT_BIT_10: u8 = 0x10;
const RESULT_BIT_11: u8 = 0x20;
const NOT_BUSY: u8 = 0x40;
const NOT_COMPLETE: u8 = 0x80;

/// The chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adc {
    control: u8,
    /// The last result, twelve bits left-justified in sixteen.
    result: u16,
    /// The cycle the conversion in progress completes.
    converting: Option<u64>,
    /// Each channel's input, full scale at `0xFFFF`.
    inputs: [u16; ANALOGUE_CHANNELS],
}

impl Default for Adc {
    fn default() -> Self {
        Self::new()
    }
}

impl Adc {
    /// A converter at power-on, every input at mid-scale as a centred
    /// joystick holds it.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            control: 0,
            result: 0,
            converting: None,
            inputs: [0x8000; ANALOGUE_CHANNELS],
        }
    }

    /// Set a channel's input, full scale at `0xFFFF`.
    pub fn set_input(&mut self, channel: usize, level: u16) {
        if let Some(input) = self.inputs.get_mut(channel) {
            *input = level;
        }
    }

    /// The end-of-conversion line: high while a conversion runs.
    #[must_use]
    pub const fn eoc(&self) -> bool {
        self.converting.is_some()
    }

    /// When the conversion in progress completes, if one is.
    #[must_use]
    pub const fn next_event(&self) -> Option<u64> {
        self.converting
    }

    /// Complete a conversion due by `cycle`.
    pub fn advance(&mut self, cycle: u64) {
        if self.converting.is_some_and(|done| done <= cycle) {
            self.converting = None;
            let input = self.inputs[usize::from(self.control & CHANNEL)];
            let precision = if self.control & TEN_BIT != 0 {
                TEN_BIT_PRECISION
            } else {
                EIGHT_BIT_PRECISION
            };
            self.result = input & precision;
        }
    }

    /// The CPU writes the control register, starting a conversion.
    pub fn write(&mut self, cycle: u64, value: u8) {
        self.advance(cycle);
        self.control = value & (CHANNEL | FLAG | TEN_BIT);
        let length = if value & TEN_BIT != 0 {
            TEN_BIT_CYCLES
        } else {
            EIGHT_BIT_CYCLES
        };
        self.converting = Some(cycle + length);
    }

    /// The CPU reads register `register` (0–3): status, the result's high
    /// byte, its low byte.
    pub fn read(&mut self, cycle: u64, register: u8) -> u8 {
        self.advance(cycle);
        let [low, high] = self.result.to_le_bytes();
        match register & 0x03 {
            register::STATUS => {
                let mut status = self.control & (CHANNEL | FLAG | TEN_BIT);
                if high & 0x40 != 0 {
                    status |= RESULT_BIT_10;
                }
                if high & 0x80 != 0 {
                    status |= RESULT_BIT_11;
                }
                if self.converting.is_some() {
                    status |= NOT_COMPLETE;
                } else {
                    status |= NOT_BUSY;
                }
                status
            }
            register::HIGH => high,
            register::LOW => low & 0xF0,
            _ => 0,
        }
    }
}

crate::snapshot::fields!(
    Adc {
        control,
        result,
        converting,
        inputs
    },
    check | adc,
    r | adc.control & !(CHANNEL | FLAG | TEN_BIT) == 0
        && adc.result & !TEN_BIT_PRECISION == 0
        && adc
            .converting
            .is_none_or(|done| r.pending(done) && done - r.clock() <= TEN_BIT_CYCLES)
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversion_takes_4_ms_at_8_bits_and_10_ms_at_10_with_eoc_high_meanwhile() {
        let mut adc = Adc::new();
        adc.write(1_000, 0x00);
        assert!(adc.eoc());
        assert_eq!(adc.next_event(), Some(9_000));
        assert_eq!(adc.read(8_999, 0) & (NOT_COMPLETE | NOT_BUSY), NOT_COMPLETE);
        assert_eq!(adc.read(9_000, 0) & (NOT_COMPLETE | NOT_BUSY), NOT_BUSY);
        assert!(!adc.eoc());
        adc.write(10_000, TEN_BIT);
        assert_eq!(adc.next_event(), Some(30_000));
    }

    #[test]
    fn the_result_is_the_selected_channel_at_the_conversion_s_precision() {
        let mut adc = Adc::new();
        adc.set_input(2, 0xABCD);
        adc.write(0, 0x02);
        adc.advance(EIGHT_BIT_CYCLES);
        assert_eq!(
            (adc.read(EIGHT_BIT_CYCLES, 1), adc.read(EIGHT_BIT_CYCLES, 2)),
            (0xAB, 0x00)
        );
        adc.write(EIGHT_BIT_CYCLES, 0x02 | TEN_BIT);
        let done = EIGHT_BIT_CYCLES + TEN_BIT_CYCLES;
        assert_eq!((adc.read(done, 1), adc.read(done, 2)), (0xAB, 0xC0));
        // The status carries the result's top two bits: 1 then 0.
        assert_eq!(
            adc.read(done, 0) & (RESULT_BIT_11 | RESULT_BIT_10),
            RESULT_BIT_11
        );
        assert_eq!(adc.read(done, 3), 0);
    }

    #[test]
    fn the_status_shows_the_channel_flag_and_resolution_written() {
        let mut adc = Adc::new();
        adc.write(0, 0xFF);
        assert_eq!(adc.read(0, 0) & 0x0F, CHANNEL | FLAG | TEN_BIT);
    }

    #[test]
    fn a_write_during_a_conversion_starts_another() {
        let mut adc = Adc::new();
        adc.set_input(0, 0x1000);
        adc.set_input(1, 0x2000);
        adc.write(0, 0x00);
        adc.write(5_000, 0x01);
        assert_eq!(adc.next_event(), Some(5_000 + EIGHT_BIT_CYCLES));
        adc.advance(13_000);
        assert_eq!(adc.read(13_000, 1), 0x20);
    }

    #[test]
    fn inputs_start_centred_and_a_channel_beyond_four_is_ignored() {
        let mut adc = Adc::new();
        adc.set_input(4, 0xFFFF);
        adc.write(0, 0x03);
        adc.advance(EIGHT_BIT_CYCLES);
        assert_eq!(adc.read(EIGHT_BIT_CYCLES, 1), 0x80);
    }
}
