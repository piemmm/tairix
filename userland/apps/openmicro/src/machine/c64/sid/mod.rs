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

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_util::mathf;

use crate::machine::lane::Device;
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

/// What the chip's sound comes from: its voices, the filter and the output
/// stage, clocked a cycle at a time.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Voices {
    /// The cycle the chip has been clocked to.
    now: u64,
    waves: [Wave; 3],
    envelopes: [Envelope; 3],
    filter: Filter,
    output: Output,
    /// The sample being taken: its sum and the cycles summed.
    sum: f64,
    summed: u8,
}

/// What the machine does to its SID's voices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidInput {
    /// A write of `value` to `register` (its low five bits).
    Write {
        /// The register.
        register: u8,
        /// The value.
        value: u8,
    },
    /// RES: the registers cleared and the envelopes released.
    Reset,
}

/// Machine cycles a runner clocks the chip by at a time: how long the machine
/// waits at most for a runner to give way.
const PIECE: u64 = 256;

/// A SID's voices, filter and output stage, and the sound they make: the part
/// of the chip a thread of its own may clock.
#[derive(Debug)]
pub struct Sid {
    tables: Arc<ModelTables>,
    clock: u64,
    rates: OutputRates,
    model: Model,
    voices: Voices,
    /// Samples made since the machine last took them, and how many more may
    /// be kept before it takes them.
    audio: Vec<f32>,
    room: usize,
}

impl Sid {
    /// A `model` chip after reset, on a machine clocked at `clock`, keeping
    /// no sound.
    #[must_use]
    pub fn new(model: Model, clock: u64) -> Self {
        let tables = ModelTables {
            dac: Tables::new(model),
            cutoff: filter::cutoff_curve(model),
        };
        let mut filter = Filter::new();
        filter.write_cutoff_low(0, &tables.cutoff, clock);
        Self {
            tables: Arc::new(tables),
            clock,
            rates: OutputRates::at(clock),
            model,
            voices: Voices {
                now: 0,
                waves: [Wave::new(); 3],
                envelopes: [Envelope::new(); 3],
                filter,
                output: Output::new(),
                sum: 0.0,
                summed: 0,
            },
            audio: Vec::new(),
            room: 0,
        }
    }

    /// Which chip it is.
    #[must_use]
    pub const fn model(&self) -> Model {
        self.model
    }

    /// Everything the chip holds, its data bus and paddles as `port` holds
    /// them, as a snapshot records it.
    #[must_use]
    pub fn state(&self, port: &mut SidPort) -> SidState {
        let v = &self.voices;
        port.sample(v.now);
        let bus_life = port.fades_at.saturating_sub(v.now);
        let (bus, bus_life) = match u32::try_from(bus_life) {
            Ok(life) if life > 0 => (port.bus, life),
            _ => (0, 0),
        };
        SidState {
            model: self.model,
            now: v.now,
            waves: v.waves,
            envelopes: v.envelopes,
            filter: v.filter,
            output: v.output,
            bus,
            bus_life,
            pot_inputs: port.pot_inputs,
            pots: port.pots,
            sum: v.sum,
            summed: v.summed,
        }
    }

    /// This chip as `state` holds it, its filter tuned again from the
    /// registers, with the port it was read through; a state of the other
    /// chip is refused. The sound not yet taken and what may be kept go with
    /// the chip it replaces.
    ///
    /// # Errors
    ///
    /// [`snapshot::SnapshotError::Model`] for the other chip's state.
    pub fn restored(&self, state: &SidState) -> Result<(Self, SidPort), snapshot::SnapshotError> {
        if state.model.number() != self.model.number() {
            return Err(snapshot::SnapshotError::Model);
        }
        let mut voices = Voices {
            now: state.now,
            waves: state.waves,
            envelopes: state.envelopes,
            filter: state.filter,
            output: state.output,
            sum: state.sum,
            summed: state.summed,
        };
        voices.filter.retune(&self.tables.cutoff, self.clock);
        let chip = Self {
            tables: Arc::clone(&self.tables),
            clock: self.clock,
            rates: self.rates,
            model: self.model,
            voices,
            audio: Vec::new(),
            room: self.room,
        };
        let port = SidPort {
            model: self.model,
            bus: state.bus,
            fades_at: state.now.saturating_add(u64::from(state.bus_life)),
            pot_inputs: state.pot_inputs,
            pots: state.pots,
            sampled_to: state.now,
        };
        Ok((chip, port))
    }

    /// RES at cycle `now`: the registers cleared and the envelopes
    /// released; the oscillators and envelope counters keep their values.
    fn reset(&mut self, now: u64) {
        self.advance(now);
        let v = &mut self.voices;
        for wave in &mut v.waves {
            wave.reset();
        }
        for envelope in &mut v.envelopes {
            envelope.reset();
        }
        let mut filter = Filter::new();
        filter.write_cutoff_low(0, &self.tables.cutoff, self.clock);
        v.filter = filter;
    }

    /// Keep up to `room` more samples before the machine next takes them.
    pub const fn keep(&mut self, room: usize) {
        self.room = room;
    }

    /// Move the samples made since last asked to the end of `into`.
    pub fn take_audio(&mut self, into: &mut Vec<f32>) {
        into.append(&mut self.audio);
    }

    /// Clock the chip up to cycle `to`, each sample finished kept while there
    /// is room for it.
    pub fn advance(&mut self, to: u64) {
        while self.voices.now < to {
            self.voices.sum += self.clock_cycle();
            self.voices.summed += 1;
            if u64::from(self.voices.summed) == CYCLES_PER_SAMPLE {
                if self.room > 0 {
                    let mean = self.voices.sum / f64::from(self.voices.summed);
                    self.audio.push(mathf::single(mean / FULL_SCALE));
                    self.room -= 1;
                }
                self.voices.sum = 0.0;
                self.voices.summed = 0;
            }
            self.voices.now += 1;
        }
    }

    /// One cycle: the output stage's level after it.
    fn clock_cycle(&mut self) -> f64 {
        let model = self.model;
        let tables = &self.tables;
        let s = &mut self.voices;
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
        let mixed = s.filter.clock(voices, model.mixer_dc());
        s.output.clock(mixed, self.rates)
    }

    /// What a read of `register` finds of voice 3 once the chip is clocked
    /// to the read: its waveform's top eight bits at OSC3, its envelope at
    /// ENV3; `None` for any other register.
    #[must_use]
    pub fn voice3(&self, register: u8) -> Option<u8> {
        match register & 0x1F {
            reg::OSC3 => Some(self.voices.waves[2].osc3()),
            reg::ENV3 => Some(self.voices.envelopes[2].env3()),
            _ => None,
        }
    }

    /// A write of `value` to `register` (its low five bits) in cycle `now`.
    fn write(&mut self, now: u64, register: u8, value: u8) {
        self.advance(now);
        let (tables, clock, model) = (&self.tables, self.clock, self.model);
        let s = &mut self.voices;
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
                    wave.write_control(model, value);
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

impl Device for Sid {
    type Input = SidInput;

    fn catch_up(&mut self, to: u64) {
        self.advance(to);
    }

    fn run_ahead(&mut self, limit: u64) -> bool {
        if self.voices.now >= limit {
            return false;
        }
        self.advance(limit.min(self.voices.now + PIECE));
        true
    }

    fn take(&mut self, at: u64, input: SidInput) {
        match input {
            SidInput::Write { register, value } => self.write(at, register, value),
            SidInput::Reset => self.reset(at),
        }
    }
}

/// The chip's data bus and its paddle readings: what a read finds that no
/// sound depends on, kept with the machine so such a read waits on no thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidPort {
    model: Model,
    /// The last value on the chip's data bus, and the cycle from which it
    /// reads as nothing.
    bus: u8,
    fades_at: u64,
    /// The paddles' positions, and the readings last taken of them.
    pot_inputs: [u8; 2],
    pots: [u8; 2],
    /// The cycle up to which the readings have been taken.
    sampled_to: u64,
}

impl SidPort {
    /// A `model` chip's port after power-on: nothing on the bus, the
    /// paddles pulled high.
    #[must_use]
    pub const fn new(model: Model) -> Self {
        Self {
            model,
            bus: 0,
            fades_at: 0,
            pot_inputs: [0xFF; 2],
            pots: [0xFF; 2],
            sampled_to: 0,
        }
    }

    /// Which chip it is.
    #[must_use]
    pub const fn model(&self) -> Model {
        self.model
    }

    /// The paddles read at every multiple of [`POT_PERIOD`] before `now`.
    fn sample(&mut self, now: u64) {
        if now <= self.sampled_to {
            return;
        }
        if self.sampled_to.next_multiple_of(POT_PERIOD) < now {
            self.pots = self.pot_inputs;
        }
        self.sampled_to = now;
    }

    /// The paddles at `x` and `y` from cycle `now`, from 0 to 255.
    pub fn set_pots(&mut self, now: u64, x: u8, y: u8) {
        self.sample(now);
        self.pot_inputs = [x, y];
    }

    /// What a read of `register` (its low five bits) in cycle `now` finds,
    /// but for voice 3's registers: a paddle's reading, or the data bus.
    pub fn read(&mut self, now: u64, register: u8) -> u8 {
        match register & 0x1F {
            reg::POT_X => {
                self.sample(now);
                self.pots[0]
            }
            reg::POT_Y => {
                self.sample(now);
                self.pots[1]
            }
            _ if now < self.fades_at => self.bus,
            _ => 0,
        }
    }

    /// `value` left on the data bus by an access in cycle `now`.
    pub fn latch(&mut self, now: u64, value: u8) {
        self.bus = value;
        self.fades_at = now.saturating_add(u64::from(self.model.bus_life()));
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
