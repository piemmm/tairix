//! The Acorn Electron: its memory map, its ULA's registers, the paging rule,
//! RAM contention, interrupts, the cassette interface, the keyboard and the
//! Plus 1 (Electron Advanced User Guide chapters 13–15).

pub mod keyboard;
pub mod plus1;
pub mod sound;
pub mod video;

#[cfg(test)]
mod tests;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::RangeInclusive;

use crate::cpu::{Bus, Cpu, CpuState};
use crate::machine::canvas::DirtyRows;
use crate::machine::cassette::{Cassette, Event as CassetteEvent, EIGHT_N_ONE};
use crate::machine::{
    load_fitting, one_mhz_stretch, save_fitting, IoAccess, Model, Monitor, Rom, RomError, Slot,
    AUDIO_CAPACITY, CYCLES_PER_SAMPLE, IO_BASE, IO_END, OS_BASE, RAM_SIZE, SHEILA, SIDEWAYS_BASE,
    SLOTS,
};
use crate::media::tape::Tape;
use crate::snapshot::{self, Reader, SnapshotError, Writer};
use keyboard::{Key, Keyboard};
use plus1::Plus1;
use sound::Sound;
use video::{Video, DISPLAY_LINES, FRAME_CYCLES, LINE_CYCLES};

/// The ULA's registers, which repeat through SHEILA every 16 bytes.
pub mod sheila {
    pub use crate::machine::SHEILA;
    /// Interrupt status (read) and enables (write).
    pub const STATUS: u16 = SHEILA;
    /// The screen start's low part: A6–A8 from bits 5–7.
    pub const START_LOW: u16 = SHEILA + 0x02;
    /// The screen start's high part: A9–A14 from bits 0–5.
    pub const START_HIGH: u16 = SHEILA + 0x03;
    /// The cassette's data register.
    pub const CASSETTE: u16 = SHEILA + 0x04;
    /// The ROM number, and the interrupts' clears.
    pub const PAGING: u16 = SHEILA + 0x05;
    /// The counter.
    pub const COUNTER: u16 = SHEILA + 0x06;
    /// The counter's use, the mode, the motor relay and the CAPS LOCK light.
    pub const CONTROL: u16 = SHEILA + 0x07;
    /// The first of the eight palette registers.
    pub const PALETTE: u16 = SHEILA + 0x08;
    /// The last of them.
    pub const LAST_PALETTE: u16 = PALETTE + 7;
}

/// The interrupt status register's bits.
pub mod irq {
    /// An enabled condition is present.
    pub const MASTER: u8 = 0x01;
    /// Power came on; cleared by the first read.
    pub const POWER_ON: u8 = 0x02;
    /// The last display line has been drawn.
    pub const DISPLAY_END: u8 = 0x04;
    /// The real-time clock's tick, at the end of line 100.
    pub const RTC: u8 = 0x08;
    /// A byte has arrived from the cassette.
    pub const RECEIVE_FULL: u8 = 0x10;
    /// The last byte to the cassette has gone.
    pub const TRANSMIT_EMPTY: u8 = 0x20;
    /// The cassette's carrier has been heard.
    pub const HIGH_TONE: u8 = 0x40;
    /// Always set.
    pub const SET: u8 = 0x80;
    /// The conditions an enable masks.
    pub const CONDITIONS: u8 = 0x7C;
}

/// `&FE05`'s bits.
pub mod paging {
    /// The ROM number.
    pub const ROM: u8 = 0x0F;
    /// Clear display end.
    pub const CLEAR_DISPLAY_END: u8 = 0x10;
    /// Clear RTC.
    pub const CLEAR_RTC: u8 = 0x20;
    /// Clear high tone.
    pub const CLEAR_HIGH_TONE: u8 = 0x40;
}

/// `&FE07`'s fields.
pub mod control {
    /// The counter's use.
    pub const COMMS: u8 = 0x06;
    /// The counter receives from the cassette.
    pub const CASSETTE_INPUT: u8 = 0x00;
    /// The counter makes sound.
    pub const SOUND: u8 = 0x02;
    /// The counter sends to the cassette.
    pub const CASSETTE_OUTPUT: u8 = 0x04;
    /// The mode, 0–7, 7 being 4.
    pub const MODE: u8 = 0x38;
    /// The cassette motor relay.
    pub const MOTOR: u8 = 0x40;
    /// The CAPS LOCK light.
    pub const CAPS_LOCK: u8 = 0x80;
}

/// The ROM numbers that read the keyboard.
pub const KEYBOARD_ROMS: RangeInclusive<u8> = 8..=9;
/// The ROM numbers BASIC answers to.
pub const BASIC_ROMS: [usize; 2] = [10, 11];
/// The keyboard's and BASIC's numbers: while one of them is paged only
/// another of 8–15 may be.
pub const LOCKED_ROMS: RangeInclusive<u8> = 8..=11;
/// A ROM number any paged ROM may be left for, and that may be left for any.
pub const FREE_ROM: u8 = 12;
const _: () = assert!(FREE_ROM > *LOCKED_ROMS.end());
/// The ROM number the Plus 1's expansion ROM answers to.
pub const PLUS1_ROM: usize = 12;
/// The ROM numbers of each of the Plus 1's cartridge slots, the first slot
/// first.
pub const CARTRIDGE_ROMS: [[usize; 2]; 2] = [[2, 3], [0, 1]];

/// Where in a frame RTC and display end are raised: the ends of lines 100
/// and 255.
const RTC_AT: u64 = 101 * LINE_CYCLES;
const DISPLAY_END_AT: u64 = DISPLAY_LINES * LINE_CYCLES;

/// The cassette's bit rate, 1 MHz / 832, to the nearest whole rate, and the
/// cycles each bit and each sent frame take.
const BAUD: u64 = 1202;
const BIT_CYCLES: u64 = 1664;
const FRAME: u64 = 10 * BIT_CYCLES;

/// The machine's state other than the CPU's.
pub struct Board {
    now: u64,
    ram: Box<[u8]>,
    os: Rom,
    slots: [Slot; SLOTS],
    /// The ROM number paged.
    rom: u8,
    /// The byte last carried by the data bus, which an undriven read sees.
    bus: u8,
    /// The conditions present, power-on among them, and those enabled.
    status: u8,
    enables: u8,
    /// Every frame time before this has raised its condition.
    raised_before: u64,
    /// `&FE07` as last written.
    control: u8,
    video: Video,
    sound: Sound,
    audio: Vec<f32>,
    audio_enabled: bool,
    keyboard: Keyboard,
    cassette: Cassette,
    received: u8,
    /// The byte being sent and the cycle its frame ends, and one written
    /// after it, sent next.
    sending: Option<(u8, u64)>,
    queued: Option<u8>,
    plus1: Option<Plus1>,
    monitor: Monitor,
    irq: bool,
    next_event: u64,
}

impl Board {
    fn new(os: Rom) -> Self {
        Self {
            now: 0,
            ram: vec![0; RAM_SIZE].into_boxed_slice(),
            os,
            slots: Default::default(),
            rom: 0,
            bus: 0,
            status: irq::POWER_ON,
            enables: 0,
            raised_before: 0,
            control: 0,
            video: Video::new(),
            sound: Sound::new(),
            audio: Vec::new(),
            audio_enabled: true,
            keyboard: Keyboard::new(),
            cassette: Cassette::new(),
            received: 0,
            sending: None,
            queued: None,
            plus1: None,
            monitor: Monitor::default(),
            irq: false,
            next_event: 0,
        }
    }

    /// End the current cycle: run what falls due in it, then count it.
    fn end_cycle(&mut self) {
        if self.now >= self.next_event {
            self.run_events();
        }
        self.now += 1;
    }

    /// Let `cycles` pass with the CPU held off the bus.
    fn wait(&mut self, cycles: u64) {
        let end = self.now + cycles;
        while self.now < end {
            if self.now >= self.next_event {
                self.run_events();
            }
            self.now = self.next_event.clamp(self.now + 1, end);
        }
    }

    /// Run everything due by the current cycle.
    fn run_events(&mut self) {
        let now = self.now;
        self.raise_frame_conditions(now);
        self.run_cassette(now);
        self.device_changed();
    }

    /// Raise RTC and display end for each of their times since last raised.
    fn raise_frame_conditions(&mut self, now: u64) {
        let mut at = self.raised_before;
        while let Some(time) = next_frame_time(at).filter(|&time| time <= now) {
            self.status |= if time % FRAME_CYCLES == RTC_AT {
                irq::RTC
            } else {
                irq::DISPLAY_END
            };
            at = time + 1;
        }
        self.raised_before = self.raised_before.max(now + 1);
    }

    /// Run the cassette to `now`: what the tape plays reaches the ULA while
    /// the counter listens to it, and what the ULA sends is recorded.
    fn run_cassette(&mut self, now: u64) {
        let listening = self.control & control::COMMS == control::CASSETTE_INPUT;
        let (status, received) = (&mut self.status, &mut self.received);
        self.cassette
            .advance(now, BAUD, EIGHT_N_ONE, listening, |event| match event {
                CassetteEvent::Frame { byte, .. } => {
                    *received = byte;
                    *status |= irq::RECEIVE_FULL;
                }
                CassetteEvent::Carrier { present: true, .. } if listening => {
                    *status |= irq::HIGH_TONE;
                }
                CassetteEvent::Carrier { .. } => {}
            });
        while let Some((byte, done)) = self.sending.filter(|&(_, done)| done <= now) {
            self.cassette.transmitted(done, byte, EIGHT_N_ONE, FRAME);
            self.sending = self.queued.take().map(|next| (next, done + FRAME));
            if self.sending.is_none() {
                self.status |= irq::TRANSMIT_EMPTY;
            }
        }
    }

    /// The next cycle anything acts of its own accord.
    fn device_events(&self) -> impl Iterator<Item = u64> {
        [
            next_frame_time(self.raised_before),
            self.cassette.next_event(BAUD, EIGHT_N_ONE),
            self.sending.map(|(_, done)| done),
        ]
        .into_iter()
        .flatten()
    }

    fn device_changed(&mut self) {
        self.irq = self.status & self.enables & irq::CONDITIONS != 0;
        let next = self.device_events().min().unwrap_or(u64::MAX);
        self.next_event = next.max(self.now + 1);
    }

    fn catch_up_video(&mut self, until: u64) {
        self.video.catch_up(until, &self.ram);
    }

    fn render_sound(&mut self, now: u64) {
        let until = now / CYCLES_PER_SAMPLE;
        if self.audio_enabled {
            let room = AUDIO_CAPACITY.saturating_sub(self.audio.len());
            let room = u64::try_from(room).unwrap_or(u64::MAX);
            let kept = until.min(self.sound.samples().saturating_add(room));
            self.sound.render(kept, &mut self.audio);
        }
        self.sound.skip(until);
    }

    fn record_io(&mut self, addr: u16, value: u8, write: bool) {
        self.monitor.record(IoAccess {
            cycle: self.now,
            addr,
            value,
            write,
        });
    }

    /// What the paged window shows at `addr`: the keyboard for ROMs 8 and 9,
    /// else the ROM paged, or the floating bus where nothing answers.
    fn paged(&self, addr: u16) -> u8 {
        if KEYBOARD_ROMS.contains(&self.rom) {
            return self.keyboard.read(addr);
        }
        self.slots[usize::from(self.rom)]
            .image()
            .map_or(self.bus, |image| image[usize::from(addr - SIDEWAYS_BASE)])
    }

    /// Page ROM `number` under the paging rule: while the keyboard or BASIC
    /// is paged, only another of 8–15 is taken.
    fn page(&mut self, number: u8) {
        if !LOCKED_ROMS.contains(&self.rom) || number >= 8 {
            self.rom = number;
        }
    }

    fn io_read(&mut self, addr: u16) -> u8 {
        let value = if addr < SHEILA {
            self.wait(one_mhz_stretch(self.now));
            let now = self.now;
            self.plus1
                .as_mut()
                .and_then(|plus1| plus1.read(now, addr))
                .unwrap_or(self.bus)
        } else {
            match SHEILA + (addr & 0x0F) {
                sheila::STATUS => {
                    let value = irq::SET | (self.status & 0x7E) | u8::from(self.irq);
                    self.status &= !irq::POWER_ON;
                    self.device_changed();
                    value
                }
                sheila::CASSETTE => {
                    self.run_cassette(self.now);
                    self.status &= !irq::RECEIVE_FULL;
                    self.device_changed();
                    self.received
                }
                // A write-only register reads as the OS beneath it.
                _ => self.os.image[usize::from(addr - OS_BASE)],
            }
        };
        self.record_io(addr, value, false);
        self.bus = value;
        self.end_cycle();
        value
    }

    fn io_write(&mut self, addr: u16, value: u8) {
        if addr < SHEILA {
            self.wait(one_mhz_stretch(self.now));
        }
        let now = self.now;
        self.record_io(addr, value, true);
        if addr < SHEILA {
            if let Some(plus1) = &mut self.plus1 {
                plus1.write(now, addr, value);
            }
        } else {
            match SHEILA + (addr & 0x0F) {
                sheila::STATUS => self.enables = value & irq::CONDITIONS,
                register @ (sheila::START_LOW | sheila::START_HIGH) => {
                    self.catch_up_video(now + 1);
                    self.video
                        .write_start(register == sheila::START_HIGH, value);
                }
                sheila::CASSETTE => self.send(now, value),
                sheila::PAGING => {
                    self.page(value & paging::ROM);
                    for (bit, condition) in [
                        (paging::CLEAR_DISPLAY_END, irq::DISPLAY_END),
                        (paging::CLEAR_RTC, irq::RTC),
                        (paging::CLEAR_HIGH_TONE, irq::HIGH_TONE),
                    ] {
                        if value & bit != 0 {
                            self.status &= !condition;
                        }
                    }
                }
                sheila::COUNTER => {
                    self.render_sound(now);
                    self.sound.set_counter(value);
                }
                sheila::CONTROL => self.write_control(now, value),
                register @ sheila::PALETTE..=sheila::LAST_PALETTE => {
                    self.catch_up_video(now + 1);
                    self.video
                        .write_palette(usize::from(register - sheila::PALETTE), value);
                }
                // &FE01 holds nothing.
                _ => {}
            }
        }
        self.device_changed();
        self.bus = value;
        self.end_cycle();
    }

    /// A byte for the cassette: sent at once if nothing is being sent, else
    /// next.
    fn send(&mut self, now: u64, byte: u8) {
        self.run_cassette(now);
        self.status &= !irq::TRANSMIT_EMPTY;
        if self.control & control::COMMS != control::CASSETTE_OUTPUT {
            return;
        }
        if self.sending.is_none() {
            self.sending = Some((byte, now + FRAME));
        } else {
            self.queued = Some(byte);
        }
    }

    fn write_control(&mut self, now: u64, value: u8) {
        self.run_cassette(now);
        self.render_sound(now);
        self.catch_up_video(now + 1);
        let was = self.control & control::COMMS;
        self.control = value;
        let comms = value & control::COMMS;
        self.video
            .write_mode((value & control::MODE) >> control::MODE.trailing_zeros());
        self.sound.set_on(comms == control::SOUND);
        self.cassette.set_motor(now, value & control::MOTOR != 0);
        if comms != was {
            if comms == control::CASSETTE_OUTPUT {
                self.status |= irq::TRANSMIT_EMPTY;
                self.cassette.set_rts(now, true);
            } else if was == control::CASSETTE_OUTPUT {
                self.sending = None;
                self.queued = None;
                self.cassette.set_rts(now, false);
            }
        }
    }

    /// Whether a restored board is one a machine could leave: nothing due
    /// already past, the cassette wired as `&FE07` drives it, and nothing
    /// being sent outside cassette output.
    fn restorable(&self) -> bool {
        let output = self.control & control::COMMS == control::CASSETTE_OUTPUT;
        self.device_events().all(|at| at > self.now)
            && self.cassette.motor() == (self.control & control::MOTOR != 0)
            && self.cassette.rts() == output
            && (output || (self.sending.is_none() && self.queued.is_none()))
            && (self.sending.is_some() || self.queued.is_none())
            && self
                .sending
                .is_none_or(|(_, done)| done - self.now <= FRAME)
    }
}

/// The first cycle at or after `from` that RTC or display end is raised.
fn next_frame_time(from: u64) -> Option<u64> {
    let frame = from - from % FRAME_CYCLES;
    [RTC_AT, DISPLAY_END_AT, FRAME_CYCLES + RTC_AT]
        .into_iter()
        .map(|offset| frame + offset)
        .find(|&time| time >= from)
}

impl Bus for Board {
    fn read(&mut self, addr: u16) -> u8 {
        let value = match addr {
            ..SIDEWAYS_BASE => {
                self.wait(self.video.ram_wait(self.now));
                self.ram[usize::from(addr)]
            }
            SIDEWAYS_BASE..OS_BASE => self.paged(addr),
            IO_BASE..IO_END => return self.io_read(addr),
            _ => self.os.image[usize::from(addr - OS_BASE)],
        };
        self.bus = value;
        self.end_cycle();
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            ..SIDEWAYS_BASE => {
                self.wait(self.video.ram_wait(self.now));
                // The ULA fetches in the half-cycle before the CPU writes.
                self.catch_up_video(self.now + 1);
                self.ram[usize::from(addr)] = value;
            }
            IO_BASE..IO_END => return self.io_write(addr, value),
            _ => {}
        }
        self.bus = value;
        self.end_cycle();
    }

    fn irq(&self) -> bool {
        self.irq
    }

    fn nmi(&self) -> bool {
        false
    }
}

/// The whole machine.
pub struct Machine {
    cpu: Cpu,
    board: Board,
    /// BREAK is held, keeping the CPU in reset.
    break_held: bool,
}

impl Machine {
    /// A machine with `os` as its operating system ROM, nothing paged in any
    /// slot and no Plus 1, powered on.
    ///
    /// # Errors
    ///
    /// [`RomError::BadSize`] for an image no socket takes.
    pub fn new(os: &[u8]) -> Result<Self, RomError> {
        let mut machine = Self {
            cpu: Cpu::new(),
            board: Board::new(Rom::of(os)?),
            break_held: false,
        };
        machine.board.device_changed();
        Ok(machine)
    }

    /// Fit the Plus 1, its ports and its sockets.
    pub fn fit_plus1(&mut self) {
        self.board.plus1.get_or_insert_with(Plus1::new);
    }

    /// Whether the Plus 1 is fitted.
    #[must_use]
    pub const fn has_plus1(&self) -> bool {
        self.board.plus1.is_some()
    }

    /// Fit `image` as BASIC, which answers to both its numbers.
    ///
    /// # Errors
    ///
    /// [`RomError::BadSize`] for an image no socket takes.
    pub fn fit_basic(&mut self, image: &[u8]) -> Result<(), RomError> {
        let basic = Rom::of(image)?;
        for slot in BASIC_ROMS {
            self.board.slots[slot] = Slot::Rom(basic.clone());
        }
        Ok(())
    }

    /// Fit a ROM image as ROM `slot` of the Plus 1: its own ROM's number or
    /// one of its cartridge slots'.
    ///
    /// # Errors
    ///
    /// [`RomError::BadSlot`] without the Plus 1 or for a number it does not
    /// answer to, [`RomError::BadSize`] for an image no socket takes.
    pub fn insert_rom(&mut self, slot: usize, image: &[u8]) -> Result<(), RomError> {
        let expansion =
            slot == PLUS1_ROM || CARTRIDGE_ROMS.iter().flatten().any(|&rom| rom == slot);
        if !(expansion && self.has_plus1()) {
            return Err(RomError::BadSlot);
        }
        self.board.slots[slot] = Slot::Rom(Rom::of(image)?);
        Ok(())
    }

    /// The current cycle.
    #[must_use]
    pub const fn cycles(&self) -> u64 {
        self.board.now
    }

    /// Run until at least `until` cycles have elapsed since power-on, ending
    /// on an instruction boundary.
    pub fn run_until(&mut self, until: u64) {
        while self.board.now < until {
            if !self.break_held {
                self.cpu.step(&mut self.board);
            } else if self.board.now < self.board.next_event {
                self.board.now = self.board.next_event.min(until);
            } else {
                self.board.end_cycle();
            }
        }
        let now = self.board.now;
        self.board.catch_up_video(now);
        self.board.render_sound(now);
    }

    /// Press or release a key.
    pub fn set_key(&mut self, key: Key, down: bool) {
        self.board.keyboard.set(key, down);
    }

    /// Release every key: what the keyboard is left with when the window
    /// loses focus mid-press.
    pub fn release_keys(&mut self) {
        self.board.keyboard.release_all();
    }

    /// Press or release BREAK, which holds the CPU in reset and restarts it
    /// as it is released. The ULA keeps its state, so the OS tells BREAK from
    /// power-on by its power-on bit.
    pub fn set_break(&mut self, down: bool) {
        if down == self.break_held {
            return;
        }
        if !down {
            self.cpu.reset();
        }
        self.break_held = down;
    }

    /// The CPU.
    #[must_use]
    pub const fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    /// A byte of RAM, read without touching the bus.
    #[must_use]
    pub fn peek(&self, addr: u16) -> u8 {
        self.board.ram.get(usize::from(addr)).copied().unwrap_or(0)
    }

    /// The display.
    #[must_use]
    pub const fn video(&self) -> &Video {
        &self.board.video
    }

    /// The rows of the finished picture that changed since last asked.
    pub fn take_dirty_rows(&mut self) -> DirtyRows {
        self.board.video.take_dirty()
    }

    /// The sound since last asked, one sample per 250 kHz update.
    pub fn take_audio(&mut self) -> alloc::vec::Drain<'_, f32> {
        self.board.audio.drain(..)
    }

    /// Keep the sound for `take_audio`, or let it run on silently, as a
    /// machine running unpaced does.
    pub fn set_sound_output(&mut self, on: bool) {
        let now = self.board.now;
        self.board.render_sound(now);
        self.board.audio_enabled = on;
    }

    /// Put `tape` in the cassette deck, wound to its start, returning the
    /// tape it replaces with what was recorded on it.
    pub fn insert_tape(&mut self, tape: Tape) -> Option<Tape> {
        let now = self.board.now;
        self.board.run_cassette(now);
        let old = self.board.cassette.insert(now, tape);
        self.board.device_changed();
        old
    }

    /// Take the tape out of the deck, with what was recorded on it.
    pub fn eject_tape(&mut self) -> Option<Tape> {
        let now = self.board.now;
        self.board.run_cassette(now);
        let tape = self.board.cassette.eject(now);
        self.board.device_changed();
        tape
    }

    /// Whether a tape is in the deck.
    #[must_use]
    pub const fn holds_tape(&self) -> bool {
        self.board.cassette.holds_tape()
    }

    /// The tape in the deck, with what has been recorded on it so far.
    pub fn tape(&mut self) -> Option<&Tape> {
        let now = self.board.now;
        self.board.cassette.tape(now)
    }

    /// Where the tape stands, in cycles from its start.
    #[must_use]
    pub fn tape_position(&self) -> u64 {
        self.board.cassette.position(self.board.now)
    }

    /// Wind the tape to `position` cycles from its start.
    pub fn wind_tape(&mut self, position: u64) {
        let now = self.board.now;
        self.board.cassette.wind(now, position);
        self.board.device_changed();
    }

    /// Press or release the deck's record key.
    pub fn set_tape_record(&mut self, record: bool) {
        let now = self.board.now;
        self.board.cassette.set_record(now, record);
        self.board.device_changed();
    }

    /// Whether the deck's record key is down, and whether the last recording
    /// ran into the tape's end.
    #[must_use]
    pub fn tape_record(&self) -> (bool, bool) {
        (self.board.cassette.recording(), self.board.cassette.full())
    }

    /// Whether the cassette motor runs: the motor light.
    #[must_use]
    pub const fn tape_motor(&self) -> bool {
        self.board.cassette.motor()
    }

    /// Whether the CAPS LOCK light is lit.
    #[must_use]
    pub const fn caps_lock_led(&self) -> bool {
        self.board.control & control::CAPS_LOCK != 0
    }

    /// Set the Plus 1's analogue channel `channel`, 0–3, full scale at
    /// `0xFFFF`; without the Plus 1 nothing reads it.
    pub fn set_analogue(&mut self, channel: usize, level: u16) {
        let now = self.board.now;
        if let Some(plus1) = &mut self.board.plus1 {
            plus1.set_input(now, channel, level);
        }
    }

    /// Hold or release the Plus 1's fire buttons, bit 0 for the first.
    pub fn set_fire_buttons(&mut self, held: u8) {
        if let Some(plus1) = &mut self.board.plus1 {
            plus1.set_fire(held);
        }
    }

    /// Start or stop a bus monitor recording accesses to the I/O pages.
    pub fn monitor_io(&mut self, on: bool) {
        self.board.monitor.set(on);
    }

    /// The accesses the bus monitor has recorded since last asked.
    pub fn take_io_trace(&mut self) -> impl Iterator<Item = IoAccess> + '_ {
        self.board.monitor.take()
    }

    /// What the Plus 1's printer port has been sent since last asked.
    pub fn take_printer_output(&mut self) -> impl Iterator<Item = u8> + '_ {
        self.board.plus1.iter_mut().flat_map(Plus1::take_output)
    }

    /// A snapshot of the machine as it stands (`crate::snapshot`), what is
    /// due in the current cycle run and the picture and sound brought up to
    /// it first, as a restore requires.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::TooLarge`] for a machine whose state outgrows a
    /// snapshot.
    pub fn snapshot(&mut self) -> Result<Vec<u8>, SnapshotError> {
        let now = self.board.now;
        self.board.catch_up_video(now);
        self.board.render_sound(now);
        self.board.run_events();
        if let Some(plus1) = &mut self.board.plus1 {
            plus1.advance(now);
        }
        let board = &self.board;
        let mut w = Writer::new();
        Model::Electron.save(&mut w);
        w.section(*b"ROMS", |w| {
            save_fitting(&board.os, &board.slots, w);
            w.put(&board.plus1.is_some());
        });
        w.section(*b"TIME", |w| w.put(&board.now));
        w.section(*b"CPU ", |w| {
            w.put(&self.cpu.state());
            w.put(&self.break_held);
        });
        w.section(*b"BORD", |w| {
            w.bytes(&board.ram);
            w.put(&board.rom);
            w.put(&board.bus);
            w.put(&board.status);
            w.put(&board.enables);
            w.put(&board.control);
            w.put(&board.received);
            w.put(&board.sending);
            w.put(&board.queued);
        });
        w.section(*b"VID ", |w| w.put(&board.video));
        w.section(*b"SND ", |w| w.put(&board.sound));
        w.section(*b"KEYS", |w| w.put(&board.keyboard));
        w.section(*b"CASS", |w| w.put(&board.cassette));
        if let Some(plus1) = &board.plus1 {
            w.section(*b"PLS1", |w| w.put(plus1));
        }
        w.finish()
    }

    /// Restore `snapshot`, taken on an Electron fitted as this one is. The
    /// whole snapshot is checked before anything changes, so a refused one
    /// leaves the machine as it was. Sound and output not yet taken belong
    /// to the machine replaced and are dropped; the sound setting and any bus
    /// monitor carry on.
    ///
    /// # Errors
    ///
    /// [`SnapshotError`]: the snapshot is damaged, holds what no machine
    /// can, or was taken on another model or one fitted otherwise.
    pub fn restore(&mut self, snapshot: &[u8]) -> Result<(), SnapshotError> {
        let body = snapshot::open(snapshot)?;
        let mut r = Reader::new(&body);
        Model::Electron.check(&mut r)?;
        let slots = r.section(*b"ROMS", |r| {
            let slots = load_fitting(&self.board.os, &self.board.slots, r)?;
            if r.get::<bool>()? != self.board.plus1.is_some() {
                return Err(SnapshotError::Fitting);
            }
            Ok(slots)
        })?;
        let now = r.section(*b"TIME", Reader::get::<u64>)?;
        r.set_clock(now)?;
        let (cpu, break_held) =
            r.section(*b"CPU ", |r| Ok((r.get::<CpuState>()?, r.get::<bool>()?)))?;
        let mut board = r.section(*b"BORD", |r| {
            let ram = r.bytes_at_most(RAM_SIZE)?;
            Reader::check(ram.len() == RAM_SIZE)?;
            let mut board = Board::new(self.board.os.clone());
            board.now = now;
            board.ram = ram.into();
            board.rom = r.get()?;
            board.bus = r.get()?;
            board.status = r.get()?;
            board.enables = r.get()?;
            board.control = r.get()?;
            board.received = r.get()?;
            board.sending = r.get()?;
            board.queued = r.get()?;
            Reader::check(
                board.rom <= paging::ROM
                    && board.status & !(irq::CONDITIONS | irq::POWER_ON) == 0
                    && board.enables & !irq::CONDITIONS == 0,
            )?;
            Ok(board)
        })?;
        board.slots = slots;
        board.raised_before = now + 1;
        board.video = r.section(*b"VID ", Reader::get)?;
        board.sound = r.section(*b"SND ", Reader::get)?;
        board.keyboard = r.section(*b"KEYS", Reader::get)?;
        board.cassette = r.section(*b"CASS", Reader::get)?;
        if self.board.plus1.is_some() {
            board.plus1 = Some(r.section(*b"PLS1", Reader::get)?);
        }
        board.audio_enabled = self.board.audio_enabled;
        board.monitor = self.board.monitor.fresh();
        Reader::check(r.left() == 0 && board.restorable())?;
        board.device_changed();
        self.cpu.restore(&cpu);
        self.board = board;
        self.break_held = break_held;
        Ok(())
    }
}
