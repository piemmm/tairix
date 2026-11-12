//! The Sound Blaster 2.0's DSP, modelled on its firmware — version 2.02 —
//! and the reverse-engineered behaviour of that firmware: the command groups
//! and what each does while a transfer runs, single-cycle, auto-initialised
//! and high-speed DMA with their block-end rules, Creative's 4-, 2.6- and
//! 2-bit ADPCM with the state the firmware carries between blocks, silence,
//! the speaker's ramps, the identification and test commands, MIDI, and the
//! reset's cold and warm paths. The card's YM3812 is [`super::opl2`].
//!
//! The DSP's DAC is unipolar and its output AC-coupled, as on the card, so
//! unmuting at code 00h and ramping to 80h is silent but for the coupling.
//! Two firmware internals are not reproduced: the on-chip RAM playback plays
//! the bytes loaded and then silence where the firmware reads memory past
//! them, and the ROM checksum and RAM test report a passing card.

use super::{Mixer, Source};
use crate::chips::dma::{Dma, DmaMemory};
use crate::time::{Hertz, Nanos};
use alloc::collections::VecDeque;

use crate::snapshot::{check, restore_queue, save_queue, In, Out, State, Torn};
use tairix_util::mathf;

/// The DMA channel the card transfers on.
pub const DMA_CHANNEL: usize = 1;

/// How long the firmware takes to read a byte the host wrote.
const TAKE: Nanos = 4_000;
/// From releasing reset to the ready byte.
const BOOT: Nanos = 50_000;
/// Each step of the speaker's ramp: the firmware's delay loop of 30h counts.
const RAMP_STEP: Nanos = 96_000;
/// The byte a reset ends with.
const READY: u8 = 0xAA;
/// The most bytes the output holds unread: the longest reply, two bytes,
/// behind the one byte a command may be taken with unread, and a reset's
/// ready byte. A ready byte that finds it full pushes out the oldest.
const OUT_DEPTH: usize = 4;
/// The firmware's version, as E1h reports it.
const VERSION: [u8; 2] = [0x02, 0x02];
/// The time constant and block size a cold reset sets.
const COLD_TIME_CONSTANT: u8 = 0x9C;
const COLD_BLOCK: u16 = 0x07FF;
/// The DMA identification's seeds.
const ID_SEEDS: [u8; 2] = [0xAA, 0x96];
/// The test tone's time constant: 62 µs a sample.
const TONE_TIME_CONSTANT: u8 = 0xC2;
/// Ticks a pass of the on-chip RAM playback lasts.
const RAM_PASS: u16 = 256;

/// The status byte 04h reports: the firmware's mode bits.
mod status {
    pub const SPEAKER: u8 = 0x01;
    pub const ADC: u8 = 0x04;
    pub const SILENCE: u8 = 0x08;
    pub const ADPCM_2: u8 = 0x10;
    pub const ADPCM_2_6: u8 = 0x20;
    pub const ADPCM_4: u8 = 0x40;
    pub const DAC_8: u8 = 0x80;
}

/// An ADPCM format: codes a byte holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Adpcm {
    #[default]
    Four,
    TwoSix,
    Two,
}

impl Adpcm {
    const fn codes(self) -> u8 {
        match self {
            Self::Four => 2,
            Self::TwoSix => 3,
            Self::Two => 4,
        }
    }

    const fn status(self) -> u8 {
        match self {
            Self::Four => status::ADPCM_4,
            Self::TwoSix => status::ADPCM_2_6,
            Self::Two => status::ADPCM_2,
        }
    }
}

/// What each tick of the sample timer does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    /// Nothing: no transfer.
    Idle,
    /// A byte through DMA to the DAC, or from the (silent) ADC to memory.
    Pcm {
        input: bool,
        high_speed: bool,
    },
    Adpcm(Adpcm),
    Silence,
    Tone,
    RamPlayback,
}

/// What the firmware does with the next host byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    Command,
    /// A command's parameters: `have` of `need` collected.
    Parameters {
        command: u8,
        have: u8,
        need: u8,
    },
    /// Samples for the on-chip RAM, `left` to come.
    RamData {
        left: u16,
        play: bool,
    },
    /// An endless loop only a reset leaves.
    Hung,
    /// MIDI input: any host write ends it, and is lost.
    MidiIn,
    /// MIDI UART mode: host bytes go out as MIDI until a reset.
    Uart,
}

/// The speaker's ramp in progress.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ramp {
    up: bool,
    next: Nanos,
}

/// The ADPCM decoder's registers, as the firmware keeps them across blocks.
#[derive(Clone, Copy, Debug, Default)]
struct Decoder {
    sample: u8,
    step: u8,
    byte: u8,
    left: u8,
}

/// The DSP.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one of the firmware's flags or lines"
)]
pub struct Dsp {
    /// The host's byte in the write latch, and when the firmware takes it.
    pending: Option<(u8, Nanos)>,
    /// Bytes for the host: the front is in the read latch, and the firmware
    /// takes nothing more while one waits behind it, so it holds at most a
    /// command's replies.
    out: VecDeque<u8>,
    last_read: u8,
    irq: bool,
    reset_held: bool,
    booting: Option<Nanos>,
    warm: bool,
    reading: Reading,
    parameters: [u8; 4],
    time_constant: u8,
    block: u16,
    /// Bytes or ticks after the current one before the block ends.
    remaining: u16,
    queued: Option<u16>,
    auto: bool,
    exit_auto: bool,
    dma_enabled: bool,
    timer: bool,
    stream: Stream,
    next_tick: Nanos,
    /// A DMA byte the firmware waits for.
    stalled: bool,
    /// An ADPCM start's first byte, still owed by the controller: whether it
    /// is the reference.
    setup: Option<bool>,
    decoder: Decoder,
    dac: u8,
    muted: bool,
    speaker_flag: bool,
    ramp: Option<Ramp>,
    /// The DMA identification's reply, waiting to go out by DMA.
    identified: Option<u8>,
    test: u8,
    id: [u8; 2],
    tone: u8,
    ram: [u8; 64],
    ram_loaded: u16,
    ram_tick: u16,
    ram_passes: u16,
    status: u8,
    /// The code the mix last heard: the DAC's, or 0 while muted.
    heard: u8,
}

impl Default for Dsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Dsp {
    /// A DSP after a cold reset, its ready byte waiting.
    #[must_use]
    pub fn new() -> Self {
        let mut dsp = Self {
            pending: None,
            out: VecDeque::new(),
            last_read: 0xFF,
            irq: false,
            reset_held: false,
            booting: None,
            warm: false,
            reading: Reading::Command,
            parameters: [0; 4],
            time_constant: COLD_TIME_CONSTANT,
            block: COLD_BLOCK,
            remaining: 0,
            queued: None,
            auto: false,
            exit_auto: false,
            dma_enabled: false,
            timer: false,
            stream: Stream::Idle,
            next_tick: 0,
            stalled: false,
            setup: None,
            decoder: Decoder::default(),
            dac: 0x80,
            muted: true,
            speaker_flag: false,
            ramp: None,
            identified: None,
            test: 0,
            id: ID_SEEDS,
            tone: 0,
            ram: [0x80; 64],
            ram_loaded: 0,
            ram_tick: 0,
            ram_passes: 0,
            status: 0,
            heard: 0,
        };
        dsp.boot(false);
        dsp
    }

    /// Whether the DSP is asking for its interrupt.
    #[must_use]
    pub const fn irq(&self) -> bool {
        self.irq
    }

    /// Whether the firmware waits on a DMA cycle the controller has not
    /// given, so that writes to the controller should retry it.
    #[must_use]
    pub const fn waiting_for_dma(&self) -> bool {
        self.stalled || self.identified.is_some()
    }

    /// The sample period the time constant sets.
    fn period(&self) -> Nanos {
        let constant = if self.stream == Stream::Tone {
            TONE_TIME_CONSTANT
        } else {
            self.time_constant
        };
        (256 - Nanos::from(constant)) * 1_000
    }

    /// Whether the firmware takes host bytes now: not in a ramp, not in a
    /// high-speed transfer, not waiting on DMA, not resetting, and not
    /// waiting for the host to empty its one output latch for a reply
    /// queued behind it.
    fn taking(&self) -> bool {
        self.out.len() <= 1
            && self.ramp.is_none()
            && !self.reset_held
            && self.booting.is_none()
            && self.identified.is_none()
            && !matches!(
                self.stream,
                Stream::Pcm {
                    high_speed: true,
                    ..
                }
            )
    }

    /// When the next thing happens.
    #[must_use]
    pub fn next_event(&self) -> Option<Nanos> {
        let mut next = self.booting;
        let mut sooner = |t: Nanos| next = Some(next.map_or(t, |n| n.min(t)));
        if let Some((_, at)) = self.pending {
            if self.taking() {
                sooner(at);
            }
        }
        if let Some(ramp) = self.ramp {
            sooner(ramp.next);
        }
        if self.timer && !self.stalled && self.ramp.is_none() && self.identified.is_none() {
            sooner(self.next_tick);
        }
        next
    }

    /// Runs everything due by `t`.
    pub fn advance<M: DmaMemory>(
        &mut self,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
        mixer: &mut Mixer,
    ) {
        if self.identified.is_some() {
            self.send_identification(dma, memory);
        }
        if self.stalled {
            // The controller may have been given the byte the firmware waits
            // for; the sample plays when it arrives.
            self.stalled = false;
            self.tick(t, dma, memory, mixer);
        }
        while let Some(at) = self.next_event().filter(|&at| at <= t) {
            if self.booting == Some(at) {
                self.booting = None;
                self.boot_ready(at, mixer);
            } else if let Some(ramp) = self.ramp.filter(|r| r.next == at) {
                self.ramp_step(ramp, mixer);
            } else if self.timer && self.next_tick == at && !self.stalled {
                self.tick(at, dma, memory, mixer);
            } else if let Some((byte, _)) = self.pending.filter(|&(_, when)| when == at) {
                self.pending = None;
                self.take(byte, at, dma, memory, mixer);
            } else {
                break;
            }
        }
    }

    /// Reads port `2x0h + offset` of the DSP's ports.
    pub fn input(&mut self, offset: u16) -> u8 {
        match offset {
            0x0A => {
                if let Some(byte) = self.out.pop_front() {
                    self.last_read = byte;
                }
                self.last_read
            }
            0x0C => 0x7F | if self.busy() { 0x80 } else { 0 },
            0x0E => {
                self.irq = false;
                0x7F | if self.out.is_empty() { 0 } else { 0x80 }
            }
            _ => 0xFF,
        }
    }

    /// The write-busy bit: an unread host byte, or the firmware busy.
    fn busy(&self) -> bool {
        self.pending.is_some()
            || self.reset_held
            || self.booting.is_some()
            || self.identified.is_some()
            || matches!(
                self.stream,
                Stream::Pcm {
                    high_speed: true,
                    ..
                }
            )
    }

    /// Writes port `2x0h + offset` at `t`.
    pub fn output(&mut self, offset: u16, value: u8, t: Nanos) {
        match offset {
            0x06 => {
                let hold = value & 1 != 0;
                if hold && !self.reset_held {
                    self.halt();
                    self.pending = None;
                    self.reading = Reading::Command;
                    self.ramp = None;
                    self.identified = None;
                    self.setup = None;
                } else if !hold && self.reset_held {
                    self.booting = Some(t + BOOT);
                }
                self.reset_held = hold;
            }
            0x0C => self.pending = Some((value, t + TAKE)),
            _ => {}
        }
    }

    /// Stops the sample timer and lets the DMA-enable line go.
    fn halt(&mut self) {
        self.timer = false;
        self.dma_enabled = false;
        self.stalled = false;
    }

    /// The reset's ready byte: a warm start keeps the settings and the
    /// speaker; a cold start restores both.
    fn boot_ready(&mut self, t: Nanos, mixer: &mut Mixer) {
        let warm = self.warm;
        self.boot(warm);
        self.set_level(t, mixer);
    }

    fn boot(&mut self, warm: bool) {
        self.warm = false;
        if warm {
            if self.speaker_flag {
                self.dac = 0x80;
                self.muted = false;
            }
        } else {
            self.dac = 0x80;
            self.time_constant = COLD_TIME_CONSTANT;
            self.block = COLD_BLOCK;
            self.id = ID_SEEDS;
            self.status = 0;
            self.speaker_flag = false;
            self.muted = true;
        }
        self.halt();
        self.stream = Stream::Idle;
        self.reading = Reading::Command;
        self.queued = None;
        self.auto = false;
        self.exit_auto = false;
        self.put_out(READY);
    }

    /// Tells the mix the DAC's level if it changed: the unipolar code, or
    /// nothing while muted.
    fn set_level(&mut self, t: Nanos, mixer: &mut Mixer) {
        let code = if self.muted { 0 } else { self.dac };
        if code != self.heard {
            let delta = (f32::from(code) - f32::from(self.heard)) / 255.0;
            mixer.step(Source::Dsp, Hertz::CRYSTAL.ticks_at(t), delta);
            self.heard = code;
        }
    }

    fn reply(&mut self, byte: u8) {
        self.put_out(byte);
    }

    fn put_out(&mut self, byte: u8) {
        if self.out.len() >= OUT_DEPTH {
            self.out.pop_front();
        }
        self.out.push_back(byte);
    }

    /// The firmware reads host byte `byte`.
    fn take<M: DmaMemory>(
        &mut self,
        byte: u8,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
        mixer: &mut Mixer,
    ) {
        match self.reading {
            Reading::Command => self.command(byte, t, dma, memory, mixer),
            Reading::Parameters {
                command,
                have,
                need,
            } => {
                self.parameters[usize::from(have)] = byte;
                if have + 1 == need {
                    self.reading = Reading::Command;
                    self.execute(command, t, dma, memory, mixer);
                } else {
                    self.reading = Reading::Parameters {
                        command,
                        have: have + 1,
                        need,
                    };
                }
            }
            Reading::RamData { left, play } => {
                let index = usize::from(self.ram_loaded);
                if let Some(slot) = self.ram.get_mut(index) {
                    *slot = byte;
                    self.ram_loaded += 1;
                }
                if left <= 1 {
                    self.reading = Reading::Command;
                    if play {
                        self.start_ram_playback(t);
                    }
                } else {
                    self.reading = Reading::RamData {
                        left: left - 1,
                        play,
                    };
                }
            }
            Reading::MidiIn => self.reading = Reading::Command,
            Reading::Hung | Reading::Uart => {}
        }
    }

    /// The parameter bytes `command` takes.
    const fn parameters_of(command: u8) -> u8 {
        match command {
            0x10..=0x13 | 0x38..=0x47 | 0xE0 | 0xE2..=0xE7 => 1,
            0x14..=0x17 | 0x24..=0x27 | 0x48..=0x4F | 0x74..=0x77 | 0x80..=0x8F => 2,
            0x58..=0x5F => 4,
            _ => 0,
        }
    }

    /// A command byte: every one first stops the sample timer, so the
    /// stream pauses; the handlers that should resume it restart it.
    fn command<M: DmaMemory>(
        &mut self,
        command: u8,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
        mixer: &mut Mixer,
    ) {
        self.timer = false;
        let group = command >> 4;
        let blocked = matches!(group, 0x4 | 0x5 | 0x8 | 0xE | 0xF)
            || (group == 0x3 && command < 0x38)
            || matches!(group, 0x6 | 0xA | 0xB | 0xC);
        if self.dma_enabled && blocked {
            match group {
                0x6 | 0xA | 0xB | 0xC if command & 0x08 != 0 => self.reading = Reading::Hung,
                0x6 | 0xA | 0xB | 0xC if command & 0x04 != 0 => {
                    let status = self.status;
                    self.reply(status);
                }
                0x6 | 0xA | 0xB | 0xC => {}
                _ => self.resume(t),
            }
            return;
        }
        if matches!(group, 0x6 | 0xA | 0xB | 0xC) {
            return;
        }
        let need = Self::parameters_of(command);
        if need > 0 {
            self.reading = Reading::Parameters {
                command,
                have: 0,
                need,
            };
            return;
        }
        self.execute(command, t, dma, memory, mixer);
    }

    /// Restarts the sample timer where the stream left off.
    fn resume(&mut self, t: Nanos) {
        if self.stream != Stream::Idle {
            self.timer = true;
            self.next_tick = self.next_tick.max(t);
        }
    }

    /// The length a command's two parameters give.
    const fn length(&self) -> u16 {
        u16::from_le_bytes([self.parameters[0], self.parameters[1]])
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a command of the firmware's table"
    )]
    fn execute<M: DmaMemory>(
        &mut self,
        command: u8,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
        mixer: &mut Mixer,
    ) {
        let parameter = self.parameters[0];
        match command {
            0x04..=0x07 => {
                let status = self.status;
                self.reply(status);
            }
            0x08..=0x0F => self.reading = Reading::Hung,
            0x10..=0x13 => {
                self.dac = parameter;
                self.set_level(t, mixer);
            }
            0x14..=0x17 | 0x24..=0x27 | 0x74..=0x77 if self.dma_enabled => self.queue(t),
            0x14 | 0x15 => self.start_pcm(false, false, Some(self.length()), t),
            0x24..=0x27 => self.start_pcm(true, false, Some(self.length()), t),
            0x18 | 0x19 | 0x1C | 0x1D => self.start_pcm(false, false, None, t),
            0x28..=0x2F => self.start_pcm(true, false, None, t),
            0x16 | 0x17 => {
                let length = Some(self.length());
                self.start_adpcm(Adpcm::Two, command & 1 != 0, length, t, dma, memory, mixer);
            }
            0x1A | 0x1B | 0x1E | 0x1F => {
                self.start_adpcm(Adpcm::Two, command & 1 != 0, None, t, dma, memory, mixer);
            }
            0x74..=0x77 => {
                let kind = if command & 2 != 0 {
                    Adpcm::TwoSix
                } else {
                    Adpcm::Four
                };
                let length = Some(self.length());
                self.start_adpcm(kind, command & 1 != 0, length, t, dma, memory, mixer);
            }
            0x70..=0x73 | 0x78..=0x7F => {
                let kind = if command & 2 != 0 {
                    Adpcm::TwoSix
                } else {
                    Adpcm::Four
                };
                self.start_adpcm(kind, command & 1 != 0, None, t, dma, memory, mixer);
            }
            0x20..=0x23 => self.reply(0x80),
            0x30..=0x33 => self.reading = Reading::MidiIn,
            0x34..=0x37 => {
                self.warm = true;
                self.reading = Reading::Uart;
            }
            0x38..=0x3F => self.resume(t),
            0x40..=0x47 => self.time_constant = parameter,
            0x48..=0x4F => self.block = self.length(),
            0x50 | 0x52 | 0x54 | 0x56 => {
                if self.stream == Stream::RamPlayback {
                    self.stream = Stream::Idle;
                }
            }
            0x51 | 0x53 | 0x55 | 0x57 => self.start_ram_playback(t),
            0x58..=0x5F => {
                // The loader takes a byte for each two counts of the first
                // parameter; the second is the passes.
                self.ram_loaded = 0;
                self.ram_passes = match self.parameters[1] {
                    0 => 256,
                    n => u16::from(n),
                };
                let left = u16::from(self.parameters[0]) / 2;
                if left == 0 {
                    if command & 1 != 0 {
                        self.start_ram_playback(t);
                    }
                } else {
                    self.reading = Reading::RamData {
                        left,
                        play: command & 1 != 0,
                    };
                }
            }
            0x80..=0x8F => {
                self.dma_enabled = true;
                self.remaining = self.length();
                self.start_stream(Stream::Silence, status::SILENCE, t);
            }
            0x90..=0x9F => {
                self.warm = true;
                self.auto = command & 1 == 0;
                self.remaining = self.block;
                self.dma_enabled = true;
                let input = command >= 0x98;
                let bit = if input { status::ADC } else { status::DAC_8 };
                self.start_stream(
                    Stream::Pcm {
                        input,
                        high_speed: true,
                    },
                    bit,
                    t,
                );
            }
            0xD0 | 0xD2 => self.halt(),
            0xD1 => self.speaker_on(t, mixer),
            0xD3 => self.speaker_off(t, mixer),
            0xD4..=0xD7 | 0xDC..=0xDF => {
                self.dma_enabled = true;
                self.resume(t);
            }
            0xD8 | 0xD9 => self.reply(if self.muted { 0x00 } else { 0xFF }),
            0xDA | 0xDB => {
                self.exit_auto = true;
                self.resume(t);
            }
            0xE0 => self.reply(!parameter),
            0xE1 => {
                self.reply(VERSION[0]);
                self.reply(VERSION[1]);
            }
            0xE2 | 0xE3 => {
                let [first, second] = self.id;
                let reply = (parameter ^ second).wrapping_add(first);
                self.id = [reply, second.rotate_right(2)];
                self.dma_enabled = true;
                self.identified = Some(reply);
                self.send_identification(dma, memory);
            }
            0xE4..=0xE7 => self.test = parameter,
            0xE8..=0xEF => {
                let test = self.test;
                self.reply(test);
            }
            0xF0 => {
                self.muted = false;
                self.set_level(t, mixer);
                self.tone = 0;
                self.start_stream(Stream::Tone, 0, t);
            }
            0xF1 => {
                let bits = u8::from(self.muted)
                    | (u8::from(!self.out.is_empty()) << 6)
                    | (u8::from(self.pending.is_some()) << 7);
                self.reply(bits);
            }
            0xF2 | 0xF3 => self.irq = true,
            0xF4..=0xF7 => {
                self.reply(0);
                self.reply(0);
            }
            0xF8..=0xFF => self.reply(0),
            _ => {}
        }
    }

    /// A single-cycle command while a transfer runs: its length is taken for
    /// the next block, and the stream resumes.
    fn queue(&mut self, t: Nanos) {
        self.queued = Some(self.length());
        self.resume(t);
    }

    fn start_stream(&mut self, stream: Stream, bit: u8, t: Nanos) {
        self.stream = stream;
        self.status = (self.status & status::SPEAKER) | bit;
        self.timer = true;
        self.stalled = false;
        self.next_tick = t + self.period();
    }

    /// 8-bit PCM over DMA, single-cycle for `length` or auto-initialised.
    fn start_pcm(&mut self, input: bool, high_speed: bool, length: Option<u16>, t: Nanos) {
        self.auto = length.is_none();
        self.exit_auto = false;
        self.queued = None;
        self.remaining = length.unwrap_or(self.block);
        self.dma_enabled = true;
        let bit = if input { status::ADC } else { status::DAC_8 };
        self.start_stream(Stream::Pcm { input, high_speed }, bit, t);
    }

    /// ADPCM over DMA: with a reference byte the decoder starts afresh from
    /// it; without one it carries on from whatever ran before.
    #[allow(
        clippy::too_many_arguments,
        reason = "the transfer's shape and the machine it runs on"
    )]
    fn start_adpcm<M: DmaMemory>(
        &mut self,
        kind: Adpcm,
        reference: bool,
        length: Option<u16>,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
        mixer: &mut Mixer,
    ) {
        self.auto = length.is_none();
        self.exit_auto = false;
        self.queued = None;
        self.remaining = length.unwrap_or(self.block);
        self.dma_enabled = true;
        self.start_stream(Stream::Adpcm(kind), kind.status(), t);
        self.setup = Some(reference);
        self.first_byte(kind, t, dma, memory, mixer);
    }

    /// The first byte of an ADPCM start, which the firmware fetches before
    /// its first tick: the reference as the sample, or the first codes.
    fn first_byte<M: DmaMemory>(
        &mut self,
        kind: Adpcm,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
        mixer: &mut Mixer,
    ) -> bool {
        let Some(reference) = self.setup else {
            return true;
        };
        if !self.fetch(dma, memory) {
            return false;
        }
        self.setup = None;
        if reference {
            self.decoder.sample = self.decoder.byte;
            self.dac = self.decoder.byte;
            self.set_level(t, mixer);
            self.decoder.step = 1;
            self.decoder.left = 1;
        } else {
            // The firmware presets four codes for the 4-bit format, so its
            // first byte plays high, low and high again.
            self.decoder.left = if kind == Adpcm::Four { 4 } else { kind.codes() };
        }
        true
    }

    fn start_ram_playback(&mut self, t: Nanos) {
        self.ram_tick = 0;
        self.start_stream(Stream::RamPlayback, 0, t);
    }

    /// One tick of the sample timer at `t`.
    fn tick<M: DmaMemory>(&mut self, t: Nanos, dma: &mut Dma, memory: &mut M, mixer: &mut Mixer) {
        self.next_tick = t + self.period();
        let host = self.pending.is_some();
        match self.stream {
            Stream::Idle => self.timer = false,
            Stream::Pcm {
                high_speed: true,
                input,
            } => self.high_speed_tick(input, t, dma, memory, mixer),
            // A host byte waiting makes a normal-mode tick do nothing.
            _ if host => {}
            Stream::Pcm { input, .. } => {
                if !self.move_byte(input, dma, memory) {
                    return self.stall(t);
                }
                self.set_level(t, mixer);
                if self.remaining == 0 {
                    self.block_end();
                } else {
                    self.remaining -= 1;
                }
            }
            Stream::Adpcm(kind) => self.adpcm_tick(kind, t, dma, memory, mixer),
            Stream::Silence => {
                if self.remaining == 0 {
                    self.finish();
                } else {
                    self.remaining -= 1;
                }
            }
            Stream::Tone => {
                self.dac = tone(self.tone);
                self.tone = self.tone.wrapping_add(1);
                self.set_level(t, mixer);
            }
            Stream::RamPlayback => {
                let index = usize::from(self.ram_tick);
                self.dac = if self.ram_tick < self.ram_loaded {
                    self.ram.get(index).copied().unwrap_or(0x80)
                } else {
                    0x80
                };
                self.set_level(t, mixer);
                self.ram_tick += 1;
                if self.ram_tick == RAM_PASS {
                    self.ram_tick = 0;
                    self.ram_passes = self.ram_passes.saturating_sub(1);
                    if self.ram_passes == 0 {
                        self.stream = Stream::Idle;
                        self.timer = false;
                    }
                }
            }
        }
    }

    /// The firmware waits for a DMA byte the controller has not given; it
    /// plays on when the controller is reprogrammed.
    fn stall(&mut self, t: Nanos) {
        self.stalled = true;
        self.next_tick = t;
    }

    /// Moves one byte between the DAC or the silent ADC and memory.
    fn move_byte<M: DmaMemory>(&mut self, input: bool, dma: &mut Dma, memory: &mut M) -> bool {
        if input {
            return dma.to_memory(DMA_CHANNEL, memory, &[0x80]).bytes == 1;
        }
        let mut byte = [0u8; 1];
        if dma.from_memory(DMA_CHANNEL, memory, &mut byte).bytes != 1 {
            return false;
        }
        self.dac = byte[0];
        true
    }

    /// A high-speed tick: a host byte waiting is taken as the sample, as
    /// the firmware cannot tell it from DMA's.
    fn high_speed_tick<M: DmaMemory>(
        &mut self,
        input: bool,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
        mixer: &mut Mixer,
    ) {
        if let (Some((byte, _)), false) = (self.pending, input) {
            self.pending = None;
            self.dac = byte;
        } else if !self.move_byte(input, dma, memory) {
            return self.stall(t);
        }
        self.set_level(t, mixer);
        if self.remaining > 0 {
            self.remaining -= 1;
            return;
        }
        self.irq = true;
        if self.auto {
            self.remaining = self.block;
        } else {
            self.warm = false;
            self.finish_quietly();
        }
    }

    /// A block's end: `DAh`'s stop, a queued length, the auto-initialised
    /// reload, or the single cycle's stop.
    fn block_end(&mut self) {
        if self.exit_auto {
            self.finish();
        } else if let Some(length) = self.queued.take() {
            self.remaining = length;
            self.auto = false;
            self.irq = true;
        } else if self.auto {
            self.remaining = self.block;
            self.irq = true;
        } else {
            self.finish();
        }
    }

    /// A transfer's end: the interrupt, and the stream stopped.
    fn finish(&mut self) {
        self.irq = true;
        self.finish_quietly();
    }

    fn finish_quietly(&mut self) {
        self.stream = Stream::Idle;
        self.status &= status::SPEAKER;
        self.auto = false;
        self.exit_auto = false;
        self.queued = None;
        self.halt();
    }

    /// One ADPCM tick: the next code of the byte in hand, or the next byte
    /// fetched and its first code, or the block's end, whose fetch plays no
    /// code on its tick.
    fn adpcm_tick<M: DmaMemory>(
        &mut self,
        kind: Adpcm,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
        mixer: &mut Mixer,
    ) {
        if self.setup.is_some() {
            if !self.first_byte(kind, t, dma, memory, mixer) {
                self.stall(t);
            }
            return;
        }
        self.decoder.left = self.decoder.left.wrapping_sub(1);
        if self.decoder.left != 0 {
            self.decode(kind);
        } else if self.remaining > 0 {
            if !self.fetch(dma, memory) {
                self.decoder.left = 1;
                return self.stall(t);
            }
            self.remaining -= 1;
            self.decoder.left = kind.codes();
            self.decode(kind);
        } else {
            if self.exit_auto || (self.queued.is_none() && !self.auto) {
                return self.finish();
            }
            // Fetched before the block's state moves on, so a retry after a
            // stall comes back to the block's end.
            if !self.fetch(dma, memory) {
                self.decoder.left = 1;
                return self.stall(t);
            }
            self.remaining = match self.queued.take() {
                Some(length) => {
                    self.auto = false;
                    length
                }
                None => self.block,
            };
            self.decoder.left = kind.codes();
            self.irq = true;
            return;
        }
        self.dac = self.decoder.sample;
        self.set_level(t, mixer);
    }

    fn fetch<M: DmaMemory>(&mut self, dma: &mut Dma, memory: &mut M) -> bool {
        let mut byte = [0u8; 1];
        if dma.from_memory(DMA_CHANNEL, memory, &mut byte).bytes != 1 {
            return false;
        }
        self.decoder.byte = byte[0];
        true
    }

    /// Decodes the code at the top of the byte in hand: a sign over a
    /// magnitude, moving the sample by the magnitude's steps and half a step,
    /// pinned at `00h` and `FFh`. A zero magnitude halves the step, the
    /// largest double it up to each format's ceiling.
    fn decode(&mut self, kind: Adpcm) {
        let d = &mut self.decoder;
        let (negative, magnitude, grows) = match kind {
            Adpcm::Four => {
                let code = d.byte >> 4;
                d.byte = d.byte.rotate_left(4);
                (code & 8 != 0, code & 7, code & 7 >= 5)
            }
            // The last code of a 2.6-bit byte is two bits, and cannot grow
            // the step.
            Adpcm::TwoSix if d.left == 1 => {
                let code = d.byte >> 6;
                (code & 2 != 0, code & 1, false)
            }
            Adpcm::TwoSix => {
                let code = d.byte >> 5;
                d.byte <<= 3;
                (code & 4 != 0, code & 3, code & 3 == 3)
            }
            Adpcm::Two => {
                let code = d.byte >> 6;
                d.byte <<= 2;
                (code & 2 != 0, code & 1, code & 1 == 1)
            }
        };
        // Eight-bit arithmetic, as the firmware's: a step carried in from
        // another format can wrap.
        let delta = magnitude.wrapping_mul(d.step).wrapping_add(d.step >> 1);
        d.sample = if negative {
            d.sample.saturating_sub(delta)
        } else {
            d.sample.saturating_add(delta)
        };
        let doubled = d.step.wrapping_mul(2);
        if magnitude == 0 {
            d.step = (d.step >> 1).max(1);
        } else if grows {
            // Each ceiling is tested for equality, so a larger step carried
            // in from another format grows on.
            d.step = match kind {
                Adpcm::Four if doubled == 0x10 => 8,
                Adpcm::TwoSix if d.step == 0x10 => 0x10,
                Adpcm::Two if d.step == 0x20 => 0x20,
                _ => doubled,
            };
        }
    }

    /// D1h: the DAC from 00h, unmuted, ramped a code at a time to 80h.
    fn speaker_on(&mut self, t: Nanos, mixer: &mut Mixer) {
        self.dac = 0;
        self.muted = false;
        self.set_level(t, mixer);
        self.ramp = Some(Ramp {
            up: true,
            next: t + RAMP_STEP,
        });
    }

    /// D3h: the DAC ramped down to 01h, then muted; at 00h it mutes at once.
    fn speaker_off(&mut self, t: Nanos, mixer: &mut Mixer) {
        if self.dac == 0 {
            self.mute(t, mixer);
            return;
        }
        self.ramp = Some(Ramp {
            up: false,
            next: t + RAMP_STEP,
        });
    }

    fn mute(&mut self, t: Nanos, mixer: &mut Mixer) {
        self.muted = true;
        self.speaker_flag = false;
        self.status &= !status::SPEAKER;
        self.set_level(t, mixer);
    }

    fn ramp_step(&mut self, ramp: Ramp, mixer: &mut Mixer) {
        let t = ramp.next;
        // The ramps count with the register the ADPCM decoder keeps its
        // codes in, which they leave at zero.
        self.decoder.left = 0;
        if ramp.up {
            self.dac += 1;
            self.set_level(t, mixer);
            if self.dac >= 0x80 {
                self.ramp = None;
                self.speaker_flag = true;
                self.status |= status::SPEAKER;
                return;
            }
        } else {
            self.dac -= 1;
            self.set_level(t, mixer);
            if self.dac <= 1 {
                self.ramp = None;
                self.mute(t, mixer);
                return;
            }
        }
        self.ramp = Some(Ramp {
            up: ramp.up,
            next: t + RAMP_STEP,
        });
    }

    /// E2h's reply goes out by DMA once the controller takes it.
    fn send_identification<M: DmaMemory>(&mut self, dma: &mut Dma, memory: &mut M) {
        let Some(reply) = self.identified else {
            return;
        };
        if dma.to_memory(DMA_CHANNEL, memory, &[reply]).bytes == 1 {
            self.identified = None;
            self.dma_enabled = false;
        }
    }
}

/// The test tone's sample `n`: every eighth entry of a 64-entry sine, a
/// cycle every eight ticks.
fn tone(n: u8) -> u8 {
    let entry = f64::from((n % 8) * 8);
    let angle = 2.0 * core::f64::consts::PI * entry / 64.0;
    u8::try_from(mathf::round_i32(128.0 + 127.0 * mathf::sin(angle))).unwrap_or(0x80)
}

crate::snapshot::state_enum!(Adpcm {
    0 => Four,
    1 => TwoSix,
    2 => Two,
});

crate::snapshot::state_enum!(Stream {
    0 => Idle,
    1 => Pcm { input, high_speed },
    2 => Adpcm(kind),
    3 => Silence,
    4 => Tone,
    5 => RamPlayback,
});

crate::snapshot::state_enum!(Reading {
    0 => Command,
    1 => Parameters { command, have, need },
    2 => RamData { left, play },
    3 => Hung,
    4 => MidiIn,
    5 => Uart,
});

crate::snapshot::state!(Ramp { up, next });

crate::snapshot::state!(Decoder {
    sample,
    step,
    byte,
    left,
});

impl State for Dsp {
    fn save(&self, out: &mut Out) {
        let Self {
            pending,
            out: unread,
            last_read,
            irq,
            reset_held,
            booting,
            warm,
            reading,
            parameters,
            time_constant,
            block,
            remaining,
            queued,
            auto,
            exit_auto,
            dma_enabled,
            timer,
            stream,
            next_tick,
            stalled,
            setup,
            decoder,
            dac,
            muted,
            speaker_flag,
            ramp,
            identified,
            test,
            id,
            tone,
            ram,
            ram_loaded,
            ram_tick,
            ram_passes,
            status,
            heard,
        } = self;
        pending.save(out);
        save_queue(unread, out);
        last_read.save(out);
        irq.save(out);
        reset_held.save(out);
        booting.save(out);
        warm.save(out);
        reading.save(out);
        parameters.save(out);
        time_constant.save(out);
        block.save(out);
        remaining.save(out);
        queued.save(out);
        auto.save(out);
        exit_auto.save(out);
        dma_enabled.save(out);
        timer.save(out);
        stream.save(out);
        next_tick.save(out);
        stalled.save(out);
        setup.save(out);
        decoder.save(out);
        dac.save(out);
        muted.save(out);
        speaker_flag.save(out);
        ramp.save(out);
        identified.save(out);
        test.save(out);
        id.save(out);
        tone.save(out);
        ram.save(out);
        ram_loaded.save(out);
        ram_tick.save(out);
        ram_passes.save(out);
        status.save(out);
        heard.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        let Self {
            pending,
            out: unread,
            last_read,
            irq,
            reset_held,
            booting,
            warm,
            reading,
            parameters,
            time_constant,
            block,
            remaining,
            queued,
            auto,
            exit_auto,
            dma_enabled,
            timer,
            stream,
            next_tick,
            stalled,
            setup,
            decoder,
            dac,
            muted,
            speaker_flag,
            ramp,
            identified,
            test,
            id,
            tone,
            ram,
            ram_loaded,
            ram_tick,
            ram_passes,
            status,
            heard,
        } = self;
        pending.restore(input)?;
        restore_queue(unread, OUT_DEPTH, input)?;
        last_read.restore(input)?;
        irq.restore(input)?;
        reset_held.restore(input)?;
        booting.restore(input)?;
        warm.restore(input)?;
        reading.restore(input)?;
        parameters.restore(input)?;
        time_constant.restore(input)?;
        block.restore(input)?;
        remaining.restore(input)?;
        queued.restore(input)?;
        auto.restore(input)?;
        exit_auto.restore(input)?;
        dma_enabled.restore(input)?;
        timer.restore(input)?;
        stream.restore(input)?;
        next_tick.restore(input)?;
        stalled.restore(input)?;
        setup.restore(input)?;
        decoder.restore(input)?;
        dac.restore(input)?;
        muted.restore(input)?;
        speaker_flag.restore(input)?;
        ramp.restore(input)?;
        identified.restore(input)?;
        test.restore(input)?;
        id.restore(input)?;
        tone.restore(input)?;
        ram.restore(input)?;
        ram_loaded.restore(input)?;
        ram_tick.restore(input)?;
        ram_passes.restore(input)?;
        status.restore(input)?;
        heard.restore(input)?;
        let collecting = match *reading {
            Reading::Parameters { have, need, .. } => {
                have < need && usize::from(need) <= parameters.len()
            }
            _ => true,
        };
        check(collecting && usize::from(*ram_loaded) <= ram.len() && *ram_tick < RAM_PASS)
    }
}

#[cfg(test)]
#[path = "blaster_tests.rs"]
mod tests;
