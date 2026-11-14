//! The Plus 1 expansion's ports in FRED: the ADC0844 behind the analogue
//! port, the parallel printer's data latch, and the status register holding
//! the printer's busy line, the end of a conversion and the fire buttons
//! (Electron Advanced User Guide §15.5–15.6). It raises no interrupt.

use alloc::vec::Vec;

use crate::machine::{collect_port, ANALOGUE_CHANNELS, FIRE_BUTTONS, FRED};

/// The ADC: a write starts a conversion, a read gives the last result.
pub const ADC: u16 = FRED + 0x70;
/// The printer's data, latched and strobed by a write.
pub const PRINTER: u16 = FRED + 0x71;
/// The status register.
pub const STATUS: u16 = FRED + 0x72;

/// What a write to the ADC starts.
pub mod command {
    /// A single-ended conversion, of the channel the low bits name.
    pub const SINGLE_ENDED: u8 = 0x04;
    /// The channel, 0–3 for the Guide's 1–4.
    pub const CHANNEL: u8 = 0x03;
}

/// Status bits.
pub mod status {
    /// The printer is busy.
    pub const BUSY: u8 = 0x80;
    /// High while a conversion runs; low once it has ended.
    pub const CONVERTING: u8 = 0x40;
    /// The second fire button, low while held.
    pub const FIRE_2: u8 = 0x20;
    /// The first fire button, low while held.
    pub const FIRE_1: u8 = 0x10;
    /// The bits nothing drives, which read high.
    pub const UNDRIVEN: u8 = 0x0F;
}

// The status register holds the fire buttons in their order, from FIRE_1.
const _: () = assert!(status::FIRE_2 == status::FIRE_1 << 1 && FIRE_BUTTONS == 0x03);

/// Cycles a conversion takes: the ADC0844's 40 µs.
pub const CONVERSION: u64 = 80;
/// Cycles the printer is busy from each strobe: 15 µs.
pub const STROBE_BUSY: u64 = 30;

/// The ports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plus1 {
    /// The channel converting, and the cycle its conversion ends.
    converting: Option<(u8, u64)>,
    result: u8,
    /// The analogue inputs, full scale at `0xFFFF`.
    inputs: [u16; ANALOGUE_CHANNELS],
    /// Fire buttons held, bit 0 for the first.
    fire: u8,
    busy_until: u64,
    output: Vec<u8>,
}

impl Plus1 {
    /// The ports at power-on: no conversion run, the printer idle.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            converting: None,
            result: 0,
            inputs: [0; ANALOGUE_CHANNELS],
            fire: 0,
            busy_until: 0,
            output: Vec::new(),
        }
    }

    /// Finish a conversion that ended by `now`, taking its channel's input.
    pub fn advance(&mut self, now: u64) {
        if let Some((channel, ends)) = self.converting {
            if now >= ends {
                let level = self.inputs[usize::from(channel)];
                self.result = level.to_be_bytes()[0];
                self.converting = None;
            }
        }
    }

    /// The CPU reads `addr` at `now`; none for an address nothing drives.
    pub fn read(&mut self, now: u64, addr: u16) -> Option<u8> {
        self.advance(now);
        match addr {
            ADC => Some(self.result),
            STATUS => {
                let held = self.fire << status::FIRE_1.trailing_zeros();
                let mut value = status::UNDRIVEN | (!held & (status::FIRE_1 | status::FIRE_2));
                if self.converting.is_some() {
                    value |= status::CONVERTING;
                }
                if now < self.busy_until {
                    value |= status::BUSY;
                }
                Some(value)
            }
            _ => None,
        }
    }

    /// The CPU writes `value` to `addr` at `now`. A write to the ADC starts a
    /// conversion of the channel its low bits name: 4–7 for channels 1–4.
    pub fn write(&mut self, now: u64, addr: u16, value: u8) {
        self.advance(now);
        match addr {
            ADC => self.converting = Some((value & command::CHANNEL, now + CONVERSION)),
            PRINTER => {
                collect_port(&mut self.output, [value]);
                self.busy_until = now + STROBE_BUSY;
            }
            _ => {}
        }
    }

    /// Set analogue channel `channel` (0–3, the Guide's 1–4) at `now`.
    pub fn set_input(&mut self, now: u64, channel: usize, level: u16) {
        self.advance(now);
        if let Some(input) = self.inputs.get_mut(channel) {
            *input = level;
        }
    }

    /// Hold or release the fire buttons, bit 0 for the first.
    pub fn set_fire(&mut self, held: u8) {
        self.fire = held & FIRE_BUTTONS;
    }

    /// What the printer has been sent since last asked.
    pub fn take_output(&mut self) -> alloc::vec::Drain<'_, u8> {
        self.output.drain(..)
    }
}

// What was printed is the front end's, taken or lost; a conversion under way
// ends within its time of the machine's, and the printer is busy no longer
// than a strobe makes it.
crate::snapshot::fields!(
    Plus1 { converting, result, inputs, fire, busy_until } transient { output },
    check | plus1, r | plus1.fire <= FIRE_BUTTONS
        && plus1
            .converting
            .is_none_or(|(channel, ends)| channel <= command::CHANNEL && r.pending(ends) && ends - r.clock() <= CONVERSION)
        && plus1.busy_until <= r.clock() + STROBE_BUSY
);

#[cfg(test)]
#[path = "plus1_tests.rs"]
mod tests;
