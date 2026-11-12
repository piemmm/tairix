//! The Yamaha YM3812 (OPL2), as the AdLib card and the Sound Blaster carry
//! it: nine two-operator FM channels, or six and five rhythm voices, run from
//! the bus's 14.318 MHz oscillator divided by four, one sample every 72
//! clocks.
//!
//! An operator is a phase generator, an envelope generator and a waveform
//! read in logarithmic form: the envelope, total level, key scaling and
//! tremolo attenuate by adding to the log-sine of the phase, and the
//! exponential table turns the sum back into a level, so the chip multiplies
//! nothing. Both tables are computed from their defining formulas; the rates,
//! depths and timers are those the YM3812 application manual documents.

use tairix_util::mathf;

/// Each register offset's operator: two a channel, with the gaps the chip
/// leaves in its map.
const SLOTS: [Option<usize>; 0x16] = [
    Some(0),
    Some(1),
    Some(2),
    Some(3),
    Some(4),
    Some(5),
    None,
    None,
    Some(6),
    Some(7),
    Some(8),
    Some(9),
    Some(10),
    Some(11),
    None,
    None,
    Some(12),
    Some(13),
    Some(14),
    Some(15),
    Some(16),
    Some(17),
];

/// A channel's two operators.
const fn operators(channel: usize) -> (usize, usize) {
    let first = (channel / 3) * 6 + channel % 3;
    (first, first + 3)
}

/// The phase generator's multiples, doubled: ½, 1, 2 … 10, 10, 12, 12, 15,
/// 15.
const MULTIPLES: [u32; 16] = [1, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 20, 24, 24, 30, 30];

/// Key scaling's attenuation by the F-number's top four bits, in 0.75 dB at
/// the top block, falling 3 dB a block below it at the 3 dB/octave slope.
const KEY_SCALE: [u16; 16] = [
    0, 32, 40, 45, 48, 51, 53, 55, 56, 58, 59, 60, 61, 62, 63, 64,
];

/// The right shift each KSL setting after the first, which scales nothing,
/// gives the 6 dB/octave attenuation: 3 dB, 1.5 dB and 6 dB an octave.
const KEY_SCALE_SHIFT: [u8; 4] = [0, 1, 2, 0];

/// The envelope's increments for the four rates of each octave: eight-step
/// patterns averaging ½, ⅝, ¾ and ⅞ of a step.
const INCREMENTS: [[u8; 8]; 4] = [
    [0, 1, 0, 1, 0, 1, 0, 1],
    [0, 1, 0, 1, 1, 1, 0, 1],
    [0, 1, 1, 1, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 1],
];

/// The envelope's quietest: 96 dB in steps of 0.1875.
const SILENT: u16 = 0x1FF;

/// Samples a step of each timer's count lasts: 80 µs and 320 µs at the
/// manual's 3.6 MHz.
const TIMER_SAMPLES: [u64; 2] = [4, 16];

/// The status register's low bits, which tell the OPL2 from the OPL3.
const STATUS_ID: u8 = 0x06;

/// The drums' keys in register `BDh`.
mod drum {
    pub const BASS: u8 = 0x10;
    pub const SNARE: u8 = 0x08;
    pub const TOM: u8 = 0x04;
    pub const CYMBAL: u8 = 0x02;
    pub const HI_HAT: u8 = 0x01;
}

/// The rhythm voices' operators.
mod rhythm {
    pub const HI_HAT: usize = 13;
    pub const TOM: usize = 14;
    pub const SNARE: usize = 16;
    pub const CYMBAL: usize = 17;
}

/// The two tables the chip carries in ROM.
#[derive(Clone, Debug)]
struct Tables {
    /// `−log2(sin)` over a quarter wave, in 256ths of a halving.
    log_sin: [u16; 256],
    /// `2^(i/256) − 1`, in 1024ths.
    exp: [u16; 256],
}

impl Tables {
    fn new() -> Self {
        let real = |i: usize| f64::from(u16::try_from(i).unwrap_or(0));
        let entry = |x: f64| u16::try_from(mathf::round_i32(x)).unwrap_or(0);
        Self {
            log_sin: core::array::from_fn(|i| {
                let angle = (real(i) + 0.5) * core::f64::consts::PI / 512.0;
                entry(-mathf::log2(mathf::sin(angle)) * 256.0)
            }),
            exp: core::array::from_fn(|i| entry((mathf::exp2(real(i) / 256.0) - 1.0) * 1024.0)),
        }
    }

    /// Waveform `wave` at the 10-bit `phase`, attenuated by `level` in
    /// 256ths of a halving: a 13-bit signed sample.
    fn wave(&self, phase: u16, level: u16, wave: u8) -> i16 {
        let mirrored = phase & 0x100 != 0;
        let second_half = phase & 0x200 != 0;
        let (silent, negative) = match wave {
            0 => (false, second_half),
            1 => (second_half, false),
            2 => (false, false),
            _ => (mirrored, false),
        };
        if silent {
            return 0;
        }
        let index = usize::from(if mirrored {
            255 - (phase & 0xFF)
        } else {
            phase & 0xFF
        });
        let total = u32::from(self.log_sin[index]) + u32::from(level);
        let shift = total >> 8;
        if shift > 12 {
            return 0;
        }
        let fraction = u8::try_from(total & 0xFF).unwrap_or(0);
        let mantissa = u32::from(self.exp[usize::from(255 - fraction)]) + 1024;
        let magnitude = i16::try_from((mantissa << 1) >> shift).unwrap_or(i16::MAX);
        if negative {
            -magnitude
        } else {
            magnitude
        }
    }
}

/// The envelope's phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Stage {
    Attack,
    Decay,
    Sustain,
    #[default]
    Release,
}

/// What keys an operator on.
mod key {
    pub const CHANNEL: u8 = 0x01;
    pub const DRUM: u8 = 0x02;
}

#[derive(Clone, Copy, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one of the operator's register bits"
)]
struct Operator {
    tremolo: bool,
    vibrato: bool,
    sustained: bool,
    key_scale_rate: bool,
    multiple: u8,
    key_scale_level: u8,
    total_level: u8,
    attack: u8,
    decay: u8,
    sustain_level: u8,
    release: u8,
    wave: u8,
    phase: u32,
    envelope: u16,
    stage: Stage,
    keys: u8,
    /// Keyed by composite sine mode for one envelope step.
    pulsed: bool,
    /// The last two outputs: the modulation the next one takes, and the
    /// feedback.
    outputs: [i16; 2],
}

impl Default for Operator {
    fn default() -> Self {
        Self {
            tremolo: false,
            vibrato: false,
            sustained: false,
            key_scale_rate: false,
            multiple: 0,
            key_scale_level: 0,
            total_level: 0,
            attack: 0,
            decay: 0,
            sustain_level: 0,
            release: 0,
            wave: 0,
            phase: 0,
            envelope: SILENT,
            stage: Stage::Release,
            keys: 0,
            pulsed: false,
            outputs: [0; 2],
        }
    }
}

impl Operator {
    /// Keys `source` on or off; the first key on restarts the attack and the
    /// phase, the last key off releases.
    fn key(&mut self, source: u8, on: bool) {
        let was = self.keys != 0;
        self.keys = if on {
            self.keys | source
        } else {
            self.keys & !source
        };
        match (was, self.keys != 0) {
            (false, true) => {
                self.stage = Stage::Attack;
                self.phase = 0;
            }
            (true, false) => self.stage = Stage::Release,
            _ => {}
        }
    }

    /// The sustain level in envelope steps: 3 dB a step, the last 93 dB.
    fn sustain_steps(&self) -> u16 {
        if self.sustain_level == 15 {
            0x1F0
        } else {
            u16::from(self.sustain_level) << 4
        }
    }

    /// The rate the envelope moves at now, key scaling added.
    fn rate(&self, scaling: u8) -> u8 {
        let base = match self.stage {
            Stage::Attack => self.attack,
            Stage::Decay => self.decay,
            Stage::Sustain if self.sustained => 0,
            Stage::Sustain | Stage::Release => self.release,
        };
        if base == 0 {
            return 0;
        }
        let scaling = if self.key_scale_rate {
            scaling
        } else {
            scaling >> 2
        };
        (base * 4 + scaling).min(63)
    }

    /// One sample of the envelope generator at global count `counter`. A
    /// composite-sine pulse attacks for this one step, then releases.
    fn advance_envelope(&mut self, scaling: u8, counter: u64) {
        let rate = self.rate(scaling);
        if rate != 0 {
            self.step_envelope(rate, counter);
        }
        if self.pulsed {
            self.pulsed = false;
            self.stage = Stage::Release;
        }
    }

    fn step_envelope(&mut self, rate: u8, counter: u64) {
        let pattern = INCREMENTS[usize::from(rate & 3)];
        let increment = if rate < 48 {
            let shift = 12 - (rate >> 2);
            if counter & ((1 << shift) - 1) != 0 {
                0
            } else {
                pattern[usize::try_from((counter >> shift) & 7).unwrap_or(0)]
            }
        } else {
            pattern[usize::try_from(counter & 7).unwrap_or(0)] << ((rate >> 2) - 12)
        };
        match self.stage {
            Stage::Attack => {
                if rate >= 60 {
                    self.envelope = 0;
                } else if increment > 0 {
                    // The attack closes a fraction of the remaining distance
                    // each step, which makes it exponential.
                    let e = i32::from(self.envelope);
                    let next = e + (((-e - 1) * i32::from(increment)) >> 3);
                    self.envelope = u16::try_from(next.max(0)).unwrap_or(0);
                }
                if self.envelope == 0 {
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.envelope = (self.envelope + u16::from(increment)).min(SILENT);
                if self.envelope >= self.sustain_steps() {
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain | Stage::Release => {
                self.envelope = (self.envelope + u16::from(increment)).min(SILENT);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Channel {
    f_number: u16,
    block: u8,
    key: bool,
    feedback: u8,
    additive: bool,
}

impl Channel {
    /// The envelope's key-scaling offset: the block and the F-number's split
    /// bit, which NTS selects.
    fn scaling(self, note_select: bool) -> u8 {
        let split = if note_select { 9 } else { 8 };
        (self.block << 1) | u8::from(self.f_number >> split & 1 != 0)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Timer {
    reload: u8,
    count: u8,
    running: bool,
    masked: bool,
}

/// The chip.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one of the chip's register bits"
)]
pub struct Opl2 {
    tables: Tables,
    address: u8,
    operators: [Operator; 18],
    channels: [Channel; 9],
    waveforms: bool,
    csm: bool,
    note_select: bool,
    deep_tremolo: bool,
    deep_vibrato: bool,
    rhythm: bool,
    drums: u8,
    timers: [Timer; 2],
    /// Status bits 6 and 5: the timers' overflows.
    flags: u8,
    noise: u32,
    /// Samples made so far, which is the chip's clock.
    samples: u64,
}

impl Default for Opl2 {
    fn default() -> Self {
        Self::new()
    }
}

impl Opl2 {
    /// A chip as power-up leaves it: every operator silent, every register
    /// clear.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tables: Tables::new(),
            address: 0,
            operators: [Operator::default(); 18],
            channels: [Channel::default(); 9],
            waveforms: false,
            csm: false,
            note_select: false,
            deep_tremolo: false,
            deep_vibrato: false,
            rhythm: false,
            drums: 0,
            timers: [Timer::default(); 2],
            flags: 0,
            noise: 1,
            samples: 0,
        }
    }

    /// The samples made so far.
    #[must_use]
    pub const fn samples(&self) -> u64 {
        self.samples
    }

    /// The samples made, from the first.
    #[must_use]
    pub const fn sampled(&self) -> u64 {
        self.samples
    }

    /// Makes the samples before sample `until`, handing each to `out` with
    /// its number.
    pub fn run_to(&mut self, until: u64, out: &mut impl FnMut(u64, i16)) {
        while self.samples < until {
            let n = self.samples;
            let sample = self.sample();
            out(n, sample);
        }
    }

    /// The status register: the IRQ bit and the two timers' flags.
    #[must_use]
    pub const fn status(&self) -> u8 {
        let irq = if self.flags != 0 { 0x80 } else { 0 };
        irq | self.flags | STATUS_ID
    }

    /// Selects the register the next data write reaches.
    pub fn select(&mut self, address: u8) {
        self.address = address;
    }

    /// Writes the selected register.
    pub fn write(&mut self, value: u8) {
        let address = self.address;
        let slot = |base: u8| SLOTS.get(usize::from(address - base)).copied().flatten();
        match address {
            0x01 => self.waveforms = value & 0x20 != 0,
            0x02 => self.timers[0].reload = value,
            0x03 => self.timers[1].reload = value,
            0x04 => self.timer_control(value),
            0x08 => {
                self.csm = value & 0x80 != 0;
                self.note_select = value & 0x40 != 0;
            }
            0x20..=0x35 => {
                if let Some(op) = slot(0x20).map(|n| &mut self.operators[n]) {
                    op.tremolo = value & 0x80 != 0;
                    op.vibrato = value & 0x40 != 0;
                    op.sustained = value & 0x20 != 0;
                    op.key_scale_rate = value & 0x10 != 0;
                    op.multiple = value & 0x0F;
                }
            }
            0x40..=0x55 => {
                if let Some(op) = slot(0x40).map(|n| &mut self.operators[n]) {
                    op.key_scale_level = value >> 6;
                    op.total_level = value & 0x3F;
                }
            }
            0x60..=0x75 => {
                if let Some(op) = slot(0x60).map(|n| &mut self.operators[n]) {
                    op.attack = value >> 4;
                    op.decay = value & 0x0F;
                }
            }
            0x80..=0x95 => {
                if let Some(op) = slot(0x80).map(|n| &mut self.operators[n]) {
                    op.sustain_level = value >> 4;
                    op.release = value & 0x0F;
                }
            }
            0xA0..=0xA8 => {
                let channel = &mut self.channels[usize::from(address - 0xA0)];
                channel.f_number = (channel.f_number & 0x300) | u16::from(value);
            }
            0xB0..=0xB8 => {
                let n = usize::from(address - 0xB0);
                let channel = &mut self.channels[n];
                channel.f_number = (channel.f_number & 0xFF) | (u16::from(value & 3) << 8);
                channel.block = (value >> 2) & 7;
                channel.key = value & 0x20 != 0;
                let key = channel.key;
                let (first, second) = operators(n);
                self.operators[first].key(key::CHANNEL, key);
                self.operators[second].key(key::CHANNEL, key);
            }
            0xBD => self.drum_control(value),
            0xC0..=0xC8 => {
                let channel = &mut self.channels[usize::from(address - 0xC0)];
                channel.feedback = (value >> 1) & 7;
                channel.additive = value & 1 != 0;
            }
            0xE0..=0xF5 => {
                if let Some(op) = slot(0xE0).map(|n| &mut self.operators[n]) {
                    op.wave = value & 3;
                }
            }
            _ => {}
        }
    }

    fn timer_control(&mut self, value: u8) {
        if value & 0x80 != 0 {
            self.flags = 0;
            return;
        }
        for (n, timer) in self.timers.iter_mut().enumerate() {
            let start = value & (1 << n) != 0;
            if start && !timer.running {
                timer.count = timer.reload;
            }
            timer.running = start;
            timer.masked = value & (0x40 >> n) != 0;
        }
    }

    fn drum_control(&mut self, value: u8) {
        self.deep_tremolo = value & 0x80 != 0;
        self.deep_vibrato = value & 0x40 != 0;
        self.rhythm = value & 0x20 != 0;
        self.drums = if self.rhythm { value & 0x1F } else { 0 };
        let keyed = |drum: u8| self.drums & drum != 0;
        let keys = [
            (12, keyed(drum::BASS)),
            (15, keyed(drum::BASS)),
            (rhythm::SNARE, keyed(drum::SNARE)),
            (rhythm::TOM, keyed(drum::TOM)),
            (rhythm::CYMBAL, keyed(drum::CYMBAL)),
            (rhythm::HI_HAT, keyed(drum::HI_HAT)),
        ];
        for (op, on) in keys {
            self.operators[op].key(key::DRUM, on);
        }
    }

    /// The tremolo's attenuation now: a triangle of 210 steps, one every 64
    /// samples, 3.7 Hz, at 4.8 dB or 1 dB.
    fn tremolo(&self) -> u16 {
        let step = (self.samples >> 6) % 210;
        let position = u16::try_from(if step < 105 { step } else { 209 - step }).unwrap_or(0);
        position >> if self.deep_tremolo { 2 } else { 4 }
    }

    /// F-number `f_number` under the vibrato now: eight steps of 1024
    /// samples, up and down by a share of its top three bits, 14 or 7 cents
    /// at the peak.
    fn vibrated(&self, f_number: u16) -> u16 {
        let position = (self.samples >> 10) & 7;
        let range = (f_number >> 7) & 7;
        let mut delta = match position & 3 {
            0 => 0,
            2 => range,
            _ => range >> 1,
        };
        if !self.deep_vibrato {
            delta >>= 1;
        }
        if position >= 4 {
            f_number.wrapping_sub(delta) & 0x3FF
        } else {
            (f_number + delta) & 0x3FF
        }
    }

    /// Advances operator `n`'s phase by one sample; returns its 10-bit
    /// phase before the step.
    fn advance_phase(&mut self, n: usize, channel: Channel) -> u16 {
        let op = self.operators[n];
        let f_number = if op.vibrato {
            self.vibrated(channel.f_number)
        } else {
            channel.f_number
        };
        let increment = (((u32::from(f_number) << channel.block) >> 1)
            * MULTIPLES[usize::from(op.multiple)])
            >> 1;
        let op = &mut self.operators[n];
        let phase = u16::try_from(op.phase >> 9).unwrap_or(0) & 0x3FF;
        op.phase = (op.phase + increment) & 0x7_FFFF;
        phase
    }

    /// Operator `n`'s attenuation now, in the envelope's steps.
    fn attenuation(&self, n: usize, channel: Channel) -> u16 {
        let op = &self.operators[n];
        let scale = if op.key_scale_level == 0 {
            0
        } else {
            let top = KEY_SCALE[usize::from(channel.f_number >> 6)] * 4;
            let below = u16::from(8 - channel.block) * 32;
            top.saturating_sub(below) >> KEY_SCALE_SHIFT[usize::from(op.key_scale_level)]
        };
        let tremolo = if op.tremolo { self.tremolo() } else { 0 };
        (op.envelope + (u16::from(op.total_level) << 2) + scale + tremolo).min(SILENT)
    }

    /// Operator `n`'s output at `phase` (its own plus any modulation), which
    /// it then keeps.
    fn output(&mut self, n: usize, phase: u16, channel: Channel) -> i16 {
        let attenuation = self.attenuation(n, channel);
        let op = self.operators[n];
        let wave = if self.waveforms { op.wave } else { 0 };
        let out = self.tables.wave(phase & 0x3FF, attenuation << 3, wave);
        let op = &mut self.operators[n];
        op.outputs = [out, op.outputs[0]];
        out
    }

    /// A channel's first operator, which modulates itself by feedback.
    fn modulator(&mut self, n: usize, phase: u16, channel: Channel) -> i16 {
        let [last, before] = self.operators[n].outputs;
        let feedback = if channel.feedback == 0 {
            0
        } else {
            (i32::from(last) + i32::from(before)) >> (9 - channel.feedback)
        };
        let modulated = i32::from(phase) + feedback;
        self.output(n, u16::try_from(modulated & 0x3FF).unwrap_or(0), channel)
    }

    /// The carrier at `phase`, modulated by `by`.
    fn carrier(&mut self, n: usize, phase: u16, by: i16, channel: Channel) -> i16 {
        let modulated = i32::from(phase) + i32::from(by);
        self.output(n, u16::try_from(modulated & 0x3FF).unwrap_or(0), channel)
    }

    /// One sample, with the clock, envelopes and timers moved on.
    fn sample(&mut self) -> i16 {
        let bit = (self.noise ^ (self.noise >> 14) ^ (self.noise >> 15) ^ (self.noise >> 22)) & 1;
        self.noise = (self.noise >> 1) | (bit << 22);
        let noise = u16::try_from(self.noise & 1).unwrap_or(0);
        let mut phases = [0u16; 18];
        for (c, channel) in self.channels.into_iter().enumerate() {
            let (first, second) = operators(c);
            phases[first] = self.advance_phase(first, channel);
            phases[second] = self.advance_phase(second, channel);
        }
        let melodic = if self.rhythm { 6 } else { 9 };
        let mut sum: i32 = 0;
        for (c, channel) in self.channels.into_iter().enumerate().take(melodic) {
            let (first, second) = operators(c);
            let modulator = self.modulator(first, phases[first], channel);
            let carrier = if channel.additive {
                self.carrier(second, phases[second], 0, channel)
            } else {
                self.carrier(second, phases[second], modulator, channel)
            };
            sum += i32::from(carrier);
            if channel.additive {
                sum += i32::from(modulator);
            }
        }
        if self.rhythm {
            sum += self.drums_sample(&phases, noise);
        }
        self.advance_envelopes();
        self.advance_timers();
        self.samples += 1;
        dac(sum)
    }

    /// The rhythm voices' sample: each played at twice a channel's level.
    fn drums_sample(&mut self, phases: &[u16; 18], noise: u16) -> i32 {
        let (bass, hats, cymbals) = (self.channels[6], self.channels[7], self.channels[8]);
        let modulator = self.modulator(12, phases[12], bass);
        let by = if bass.additive { 0 } else { modulator };
        let mut sum = i32::from(self.carrier(15, phases[15], by, bass));
        let bit = |p: u16, n: u32| (p >> n) & 1;
        let (p13, p17) = (phases[rhythm::HI_HAT], phases[rhythm::CYMBAL]);
        let ring = (bit(p13, 2) ^ bit(p13, 7)) | bit(p13, 3) | (bit(p17, 3) ^ bit(p17, 5));
        let hi_hat = (ring << 9) | if ring ^ noise != 0 { 0xD0 } else { 0x34 };
        let snare = (bit(p13, 8) << 9) | ((bit(p13, 8) ^ noise) << 8);
        let cymbal = (ring << 9) | 0x80;
        sum += i32::from(self.output(rhythm::HI_HAT, hi_hat, hats));
        sum += i32::from(self.output(rhythm::SNARE, snare, hats));
        sum += i32::from(self.output(rhythm::TOM, phases[rhythm::TOM], cymbals));
        sum += i32::from(self.output(rhythm::CYMBAL, cymbal, cymbals));
        sum * 2
    }

    fn advance_envelopes(&mut self) {
        let counter = self.samples;
        for c in 0..9 {
            let scaling = self.channels[c].scaling(self.note_select);
            let (first, second) = operators(c);
            self.operators[first].advance_envelope(scaling, counter);
            self.operators[second].advance_envelope(scaling, counter);
        }
    }

    fn advance_timers(&mut self) {
        for (n, every) in TIMER_SAMPLES.into_iter().enumerate() {
            if self.samples % every != every - 1 {
                continue;
            }
            let timer = &mut self.timers[n];
            if !timer.running {
                continue;
            }
            if timer.count == 0xFF {
                timer.count = timer.reload;
                if !timer.masked {
                    self.flags |= 0x40 >> n;
                }
                if n == 0 && self.csm {
                    self.csm_key();
                }
            } else {
                timer.count += 1;
            }
        }
    }

    /// Composite sine mode: timer 1's overflow keys every operator on and
    /// at once off again, so each attacks for one envelope step and then
    /// releases.
    fn csm_key(&mut self) {
        for op in &mut self.operators {
            if op.keys == 0 {
                op.stage = Stage::Attack;
                op.phase = 0;
                op.pulsed = true;
            }
        }
    }
}

/// The YM3014B's view of a sample: clipped to 16 bits and carried as a
/// 10-bit mantissa under a 3-bit exponent, so a loud sample loses its low
/// bits.
fn dac(sum: i32) -> i16 {
    let clipped = sum.clamp(-32768, 32767);
    let mut mantissa = clipped;
    let mut exponent = 0;
    while !(-512..512).contains(&mantissa) {
        mantissa >>= 1;
        exponent += 1;
    }
    i16::try_from(mantissa << exponent).unwrap_or(0)
}

crate::snapshot::state_enum!(Stage {
    0 => Attack,
    1 => Decay,
    2 => Sustain,
    3 => Release,
});

crate::snapshot::state!(
    Operator {
        tremolo,
        vibrato,
        sustained,
        key_scale_rate,
        multiple,
        key_scale_level,
        total_level,
        attack,
        decay,
        sustain_level,
        release,
        wave,
        phase,
        envelope,
        stage,
        keys,
        pulsed,
        outputs,
    }
    check |op: &Operator| {
        // Every field the register writes fill within the bits they write,
        // as the tables and rates it indexes rely on.
        op.multiple < 16
            && op.key_scale_level < 4
            && op.total_level < 64
            && op.attack < 16
            && op.decay < 16
            && op.sustain_level < 16
            && op.release < 16
            && op.wave < 4
            && op.envelope <= SILENT
    }
);

crate::snapshot::state!(
    Channel { f_number, block, key, feedback, additive }
    check |channel: &Channel| channel.f_number < 1024 && channel.block < 8 && channel.feedback < 8
);

crate::snapshot::state!(Timer {
    reload,
    count,
    running,
    masked,
});

crate::snapshot::state!(
    Opl2 {
        address,
        operators,
        channels,
        waveforms,
        csm,
        note_select,
        deep_tremolo,
        deep_vibrato,
        rhythm,
        drums,
        timers,
        flags,
        noise,
        samples,
    }
    as built { tables }
);

#[cfg(test)]
#[path = "opl2_tests.rs"]
mod tests;
