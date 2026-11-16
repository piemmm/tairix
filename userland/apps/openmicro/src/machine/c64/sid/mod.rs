//! The SID, 6581 and 8580: three voices — each an oscillator, its waveforms
//! and an envelope — the filter, and the C64's output stage, clocked a
//! machine cycle at a time and heard as the mean of every
//! [`CYCLES_PER_SAMPLE`] cycles.
//!
//! The chips differ in their DACs, their combined waveforms, their filter
//! curves, and the 6581's DC offsets, which let a write to the volume
//! register sound on its own.

mod dac;
mod envelope;
mod filter;
mod wave;

use alloc::rc::Rc;
use alloc::vec::Vec;

use tairix_util::mathf;

use crate::snapshot;
use dac::Tables;
use envelope::Envelope;
use filter::{Filter, Output, OutputRates};
use wave::Wave;

/// Registers, repeated through the chip's page every 32 bytes.
pub mod reg {
    /// Voice 1's frequency, low byte; each voice's registers follow seven on.
    pub const FREQUENCY_LO: u8 = 0x00;
    /// Its frequency, high byte.
    pub const FREQUENCY_HI: u8 = 0x01;
    /// Its pulse width, low byte.
    pub const PULSE_LO: u8 = 0x02;
    /// Its pulse width's top four bits.
    pub const PULSE_HI: u8 = 0x03;
    /// Its control: waveforms, test, ring, sync and gate.
    pub const CONTROL: u8 = 0x04;
    /// Its attack and decay rates.
    pub const ATTACK_DECAY: u8 = 0x05;
    /// Its sustain level and release rate.
    pub const SUSTAIN_RELEASE: u8 = 0x06;
    /// Registers a voice takes.
    pub const VOICE: u8 = 7;
    /// The cutoff's low three bits.
    pub const CUTOFF_LO: u8 = 0x15;
    /// The cutoff's high eight bits.
    pub const CUTOFF_HI: u8 = 0x16;
    /// Resonance, and which voices the filter takes.
    pub const RESONANCE: u8 = 0x17;
    /// The filter's modes, voice 3's switch, and the volume.
    pub const MODE_VOLUME: u8 = 0x18;
    /// Paddle X, read.
    pub const POT_X: u8 = 0x19;
    /// Paddle Y, read.
    pub const POT_Y: u8 = 0x1A;
    /// Voice 3's waveform's top eight bits, read.
    pub const OSC3: u8 = 0x1B;
    /// Voice 3's envelope, read.
    pub const ENV3: u8 = 0x1C;
}

/// Which chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// The original 6581.
    Mos6581,
    /// The 8580 that replaced it.
    Mos8580,
}

impl Model {
    /// Both chips, in the order they are numbered.
    pub const ALL: [Self; 2] = [Self::Mos6581, Self::Mos8580];

    /// Its number, as snapshots and messages record it.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Mos6581 => 0,
            Self::Mos8580 => 1,
        }
    }

    /// The chip numbered `number`, if there is one.
    #[must_use]
    pub fn from_number(number: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|model| model.number() == number)
    }

    /// Cycles a value written or read stays on the chip's data bus, which a
    /// read of a write-only register finds.
    const fn bus_life(self) -> u32 {
        match self {
            Self::Mos6581 => 0x1D00,
            Self::Mos8580 => 0xA_2000,
        }
    }

    /// The waveform DAC's zero, and the DC the envelope DAC adds a voice,
    /// both as raw DAC levels.
    const fn voice_offsets(self) -> (f64, f64) {
        match self {
            // $380, and $800 at the envelope's full $FF.
            Self::Mos6581 => (896.0, 522_240.0),
            Self::Mos8580 => (2048.0, 0.0),
        }
    }

    /// The mixer's DC offset: on the 6581 about an eighteenth of a voice's
    /// range, $FFF by $FF, below its zero.
    const fn mixer_dc(self) -> f64 {
        match self {
            Self::Mos6581 => -1_044_225.0 / 18.0,
            Self::Mos8580 => 0.0,
        }
    }
}

impl snapshot::Save for Model {
    fn save(&self, w: &mut snapshot::Writer) {
        w.put(&self.number());
    }
}

impl snapshot::Load for Model {
    fn load(r: &mut snapshot::Reader<'_>) -> Result<Self, snapshot::SnapshotError> {
        Self::from_number(r.get()?).ok_or(snapshot::SnapshotError::Malformed)
    }
}

/// Machine cycles a sample of the SID's sound spans.
pub const CYCLES_PER_SAMPLE: u64 = 4;

/// Cycles between the SID's paddle readings.
const POT_PERIOD: u64 = 512;

/// The level a sample of full scale reaches: three voices' full swing, $FFF
/// by $FF each, at full volume.
const FULL_SCALE: f64 = 1_044_225.0 * 3.0 * 15.0;

/// A model's tables: its DACs, its combined waveforms, its cutoff curve.
#[derive(Debug, PartialEq)]
struct ModelTables {
    dac: Tables,
    cutoff: alloc::boxed::Box<[f64]>,
}

/// Everything the chip holds, as a snapshot records it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SidState {
    model: Model,
    /// The cycle the chip has been clocked to.
    now: u64,
    waves: [Wave; 3],
    envelopes: [Envelope; 3],
    filter: Filter,
    output: Output,
    /// The last value on the chip's data bus, and the cycles it lasts.
    bus: u8,
    bus_life: u32,
    /// The paddles' positions, and the readings last taken of them.
    pot_inputs: [u8; 2],
    pots: [u8; 2],
    /// The sample being taken: its sum and the cycles summed.
    sum: f64,
    summed: u8,
}

snapshot::fields!(
    SidState {
        model,
        now,
        waves,
        envelopes,
        filter,
        output,
        bus,
        bus_life,
        pot_inputs,
        pots,
        sum,
        summed
    },
    check | s,
    r | s.now == r.clock() && u64::from(s.summed) < CYCLES_PER_SAMPLE
);

/// A SID.
#[derive(Clone, Debug)]
pub struct Sid {
    tables: Rc<ModelTables>,
    clock: u64,
    rates: OutputRates,
    state: SidState,
}

impl Sid {
    /// A `model` chip after reset, on a machine clocked at `clock`.
    #[must_use]
    pub fn new(model: Model, clock: u64) -> Self {
        let tables = ModelTables {
            dac: Tables::new(model),
            cutoff: filter::cutoff_curve(model),
        };
        let mut filter = Filter::new();
        filter.write_cutoff_low(0, &tables.cutoff, clock);
        Self {
            tables: Rc::new(tables),
            clock,
            rates: OutputRates::at(clock),
            state: SidState {
                model,
                now: 0,
                waves: [Wave::new(); 3],
                envelopes: [Envelope::new(); 3],
                filter,
                output: Output::new(),
                bus: 0,
                bus_life: 0,
                pot_inputs: [0xFF; 2],
                pots: [0xFF; 2],
                sum: 0.0,
                summed: 0,
            },
        }
    }

    /// Which chip it is.
    #[must_use]
    pub const fn model(&self) -> Model {
        self.state.model
    }

    /// Everything the chip holds.
    #[must_use]
    pub const fn state(&self) -> &SidState {
        &self.state
    }

    /// Take back a state of this chip, its filter tuned again from the
    /// registers; a state of the other chip is refused.
    ///
    /// # Errors
    ///
    /// [`snapshot::SnapshotError::Model`] for the other chip's state.
    pub fn restore(&mut self, state: SidState) -> Result<(), snapshot::SnapshotError> {
        if state.model.number() != self.state.model.number() {
            return Err(snapshot::SnapshotError::Model);
        }
        self.state = state;
        self.state.filter.retune(&self.tables.cutoff, self.clock);
        Ok(())
    }

    /// RES at cycle `now`: the registers cleared and the envelopes
    /// released; the oscillators and envelope counters keep their values.
    pub fn reset(&mut self, now: u64, audio: &mut Vec<f32>, capacity: usize) {
        self.advance(now, audio, capacity);
        for wave in &mut self.state.waves {
            wave.reset();
        }
        for envelope in &mut self.state.envelopes {
            envelope.reset();
        }
        let mut filter = Filter::new();
        filter.write_cutoff_low(0, &self.tables.cutoff, self.clock);
        self.state.filter = filter;
    }

    /// The paddles at `x` and `y`, from 0 to 255.
    pub const fn set_pots(&mut self, x: u8, y: u8) {
        self.state.pot_inputs = [x, y];
    }

    /// Clock the chip up to cycle `to`, each sample finished pushed to
    /// `audio` while it holds fewer than `capacity`.
    pub fn advance(&mut self, to: u64, audio: &mut Vec<f32>, capacity: usize) {
        while self.state.now < to {
            if self.state.now.is_multiple_of(POT_PERIOD) {
                self.state.pots = self.state.pot_inputs;
            }
            self.state.sum += self.clock_cycle();
            self.state.summed += 1;
            if u64::from(self.state.summed) == CYCLES_PER_SAMPLE {
                if audio.len() < capacity {
                    let mean = self.state.sum / f64::from(self.state.summed);
                    audio.push(mathf::single(mean / FULL_SCALE));
                }
                self.state.sum = 0.0;
                self.state.summed = 0;
            }
            self.state.now += 1;
        }
    }

    /// One cycle: the output stage's level after it.
    fn clock_cycle(&mut self) -> f64 {
        let model = self.state.model;
        let tables = &self.tables;
        let s = &mut self.state;
        for envelope in &mut s.envelopes {
            envelope.clock();
        }
        for wave in &mut s.waves {
            wave.clock(model);
        }
        // Each voice syncs the next, voice 3 the first; a source synced on
        // the cycle its own top bit rises does not pass the sync on.
        let rising = s.waves.map(|wave| wave.msb_rising());
        let synced: [bool; 3] = core::array::from_fn(|dest| {
            let source = (dest + 2) % 3;
            let behind = (source + 2) % 3;
            rising[source] && s.waves[dest].sync() && !(s.waves[source].sync() && rising[behind])
        });
        for (wave, synced) in s.waves.iter_mut().zip(synced) {
            if synced {
                wave.synchronise();
            }
        }
        for voice in 0..3 {
            let source = s.waves[(voice + 2) % 3].accumulator();
            s.waves[voice].update_output(model, &tables.dac, source);
        }
        let (zero, dc) = model.voice_offsets();
        let voices: [f64; 3] = core::array::from_fn(|voice| {
            let wave = tables.dac.wave[usize::from(s.waves[voice].output())];
            let envelope = tables.dac.envelope[usize::from(s.envelopes[voice].output())];
            (wave - zero) * envelope + dc
        });
        if s.bus_life != 0 {
            s.bus_life -= 1;
            if s.bus_life == 0 {
                s.bus = 0;
            }
        }
        let mixed = s.filter.clock(voices, model.mixer_dc());
        s.output.clock(mixed, self.rates)
    }

    /// The CPU reads `register` (its low five bits) in cycle `now`.
    pub fn read(&mut self, now: u64, register: u8, audio: &mut Vec<f32>, capacity: usize) -> u8 {
        self.advance(now, audio, capacity);
        let s = &mut self.state;
        let value = match register & 0x1F {
            reg::POT_X => s.pots[0],
            reg::POT_Y => s.pots[1],
            reg::OSC3 => s.waves[2].osc3(),
            reg::ENV3 => s.envelopes[2].env3(),
            _ => s.bus,
        };
        s.bus = value;
        s.bus_life = s.model.bus_life();
        value
    }

    /// The CPU writes `value` to `register` (its low five bits) in cycle
    /// `now`.
    pub fn write(
        &mut self,
        now: u64,
        register: u8,
        value: u8,
        audio: &mut Vec<f32>,
        capacity: usize,
    ) {
        self.advance(now, audio, capacity);
        let (tables, clock) = (&self.tables, self.clock);
        let s = &mut self.state;
        s.bus = value;
        s.bus_life = s.model.bus_life();
        let register = register & 0x1F;
        if register < 3 * reg::VOICE {
            let voice = usize::from(register / reg::VOICE);
            let (wave, envelope) = (&mut s.waves[voice], &mut s.envelopes[voice]);
            match register % reg::VOICE {
                reg::FREQUENCY_LO => wave.frequency = (wave.frequency & 0xFF00) | u16::from(value),
                reg::FREQUENCY_HI => {
                    wave.frequency = (wave.frequency & 0x00FF) | (u16::from(value) << 8);
                }
                reg::PULSE_LO => wave.width = (wave.width & 0x0F00) | u16::from(value),
                reg::PULSE_HI => {
                    wave.width = (wave.width & 0x00FF) | (u16::from(value & 0x0F) << 8);
                }
                reg::CONTROL => {
                    wave.write_control(s.model, value);
                    envelope.write_control(value);
                }
                reg::ATTACK_DECAY => envelope.write_attack_decay(value),
                _ => envelope.write_sustain_release(value),
            }
            return;
        }
        match register {
            reg::CUTOFF_LO => s.filter.write_cutoff_low(value, &tables.cutoff, clock),
            reg::CUTOFF_HI => s.filter.write_cutoff_high(value, &tables.cutoff, clock),
            reg::RESONANCE => s.filter.write_resonance_routing(value),
            reg::MODE_VOLUME => s.filter.write_mode_volume(value),
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
