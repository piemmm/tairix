//! The Commodore 64 (`plans/C64.md`): the 6510 and the board that joins its
//! memory, the VIC-II, the SID and the two CIAs.
//!
//! Each bus cycle takes the VIC-II's half first: [`Vic::begin_cycle`] has run
//! for a cycle before the CPU's access in it, and the cycle ends with the
//! chip's second half. While BA is low a CPU read waits and is made once BA
//! rises; writes go ahead, as the 6510's three-cycle warning lets them.

pub mod cartridge;
pub mod cia;
pub mod datasette;
pub mod drive;
pub mod keyboard;
pub mod memory;
pub mod palette;
pub mod sid;
pub mod vic;

use alloc::vec::{Drain, Vec};

use tairix_crypto::FINGERPRINT_LEN;

use crate::cpu::{Bus, Cpu, CpuState};
use crate::machine::canvas::DirtyRows;
use crate::machine::{IoAccess, LetterCase, Model, Monitor, RomError, Timing};
use crate::media::c1541::Surface;
use crate::media::crt::Crt;
use crate::media::prg::Program;
use crate::media::tap::Tape;
use crate::snapshot::{self, Reader, SnapshotError, Writer};
use cartridge::{Cartridge, CartridgeError};
use cia::{Cia, Mains};
use datasette::Datasette;
use drive::{Drive, Lines, Unit};
use keyboard::{Key, Keyboard, Keycap};
use memory::{vic_character_offset, Chip, Memory, LOWER_CASE_SET};
use sid::{Sid, SidState};
use vic::{Vic, VicMemory};

/// The television standard a machine is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standard {
    /// PAL: the 6569 and 50 Hz mains.
    Pal,
    /// NTSC: the 6567R8 and 60 Hz mains.
    Ntsc,
}

impl Standard {
    /// Both standards, in the order they are numbered.
    pub const ALL: [Self; 2] = [Self::Pal, Self::Ntsc];

    /// Its number, as snapshots and messages record it.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Pal => 0,
            Self::Ntsc => 1,
        }
    }

    /// The standard numbered `number`, if there is one.
    #[must_use]
    pub fn from_number(number: u8) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|standard| standard.number() == number)
    }

    /// Its VIC-II.
    #[must_use]
    pub const fn vic(self) -> vic::Model {
        match self {
            Self::Pal => vic::Model::Mos6569,
            Self::Ntsc => vic::Model::Mos6567R8,
        }
    }

    /// Machine cycles a second.
    #[must_use]
    pub const fn clock(self) -> u64 {
        self.vic().clock()
    }

    /// How a machine of this standard keeps time, its sound the SID's.
    #[must_use]
    pub const fn timing(self) -> Timing {
        const PAL: Timing = Timing::of(Standard::Pal.clock(), sid::CYCLES_PER_SAMPLE);
        const NTSC: Timing = Timing::of(Standard::Ntsc.clock(), sid::CYCLES_PER_SAMPLE);
        match self {
            Self::Pal => PAL,
            Self::Ntsc => NTSC,
        }
    }

    /// The mains the time-of-day clocks count.
    #[must_use]
    pub const fn mains(self) -> Mains {
        Mains {
            clock: self.clock(),
            hz: match self {
                Self::Pal => 50,
                Self::Ntsc => 60,
            },
        }
    }
}

impl snapshot::Save for Standard {
    fn save(&self, w: &mut snapshot::Writer) {
        w.put(&self.number());
    }
}

impl snapshot::Load for Standard {
    fn load(r: &mut snapshot::Reader<'_>) -> Result<Self, snapshot::SnapshotError> {
        Self::from_number(r.get()?).ok_or(snapshot::SnapshotError::Malformed)
    }
}

/// How a machine is built: its television standard and its sound chip.
/// The 8580's boards carry the 8521 CIA, the 6581's the 6526.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The television standard.
    pub standard: Standard,
    /// The SID.
    pub sid: sid::Model,
}

impl Config {
    const fn cia(self) -> cia::Model {
        match self.sid {
            sid::Model::Mos6581 => cia::Model::Mos6526,
            sid::Model::Mos8580 => cia::Model::Mos8521,
        }
    }
}

/// The ROM images a machine is built with.
#[derive(Clone, Copy, Debug)]
pub struct Roms<'a> {
    /// BASIC, 8 KiB.
    pub basic: &'a [u8],
    /// The KERNAL, 8 KiB.
    pub kernal: &'a [u8],
    /// The character generator, 4 KiB.
    pub character: &'a [u8],
}

/// Joystick lines, low while held: bits of the port each grounds.
pub mod joystick {
    /// Up.
    pub const UP: u8 = 0x01;
    /// Down.
    pub const DOWN: u8 = 0x02;
    /// Left.
    pub const LEFT: u8 = 0x04;
    /// Right.
    pub const RIGHT: u8 = 0x08;
    /// Fire, which on port 1 also strobes the light pen.
    pub const FIRE: u8 = 0x10;
    /// Every line.
    pub const ALL: u8 = 0x1F;
}

/// Where the I/O chips answer in the I/O page.
pub mod io {
    /// The VIC-II's registers.
    pub const VIC: u16 = 0xD000;
    /// The SID's.
    pub const SID: u16 = 0xD400;
    /// Colour RAM.
    pub const COLOUR: u16 = 0xD800;
    /// CIA1's registers.
    pub const CIA1: u16 = 0xDC00;
    /// CIA2's.
    pub const CIA2: u16 = 0xDD00;
    /// I/O1 and I/O2, the expansion port's pages.
    pub const IO1: u16 = 0xDE00;
    /// See [`IO1`].
    pub const IO2: u16 = 0xDF00;

    /// The VIC-II's register `register`.
    #[must_use]
    pub const fn vic(register: u8) -> u16 {
        VIC + register as u16
    }

    /// CIA1's register `register`.
    #[must_use]
    pub const fn cia1(register: u8) -> u16 {
        CIA1 + register as u16
    }

    /// CIA2's register `register`.
    #[must_use]
    pub const fn cia2(register: u8) -> u16 {
        CIA2 + register as u16
    }
}

/// CIA2's port A lines: the VIC-II's bank, its complement in the low two,
/// and the serial bus, driven through inverters so that a 1 pulls a line
/// low and read as the lines stand.
pub mod serial {
    /// The VIC-II's bank, inverted.
    pub const BANK: u8 = 0x03;
    /// ATN, driven.
    pub const ATN_OUT: u8 = 0x08;
    /// CLOCK, driven.
    pub const CLOCK_OUT: u8 = 0x10;
    /// DATA, driven.
    pub const DATA_OUT: u8 = 0x20;
    /// CLOCK, read: 1 while high.
    pub const CLOCK_IN: u8 = 0x40;
    /// DATA, read: 1 while high.
    pub const DATA_IN: u8 = 0x80;

    /// The commands a controller sends under ATN, which every device
    /// reads.
    pub mod command {
        /// LISTEN, a device's number added.
        pub const LISTEN: u8 = 0x20;
        /// TALK, a device's number added.
        pub const TALK: u8 = 0x40;
        /// UNLISTEN.
        pub const UNLISTEN: u8 = 0x3F;
        /// UNTALK.
        pub const UNTALK: u8 = 0x5F;
        /// A channel's data, its number added.
        pub const DATA: u8 = 0x60;
        /// A channel closed.
        pub const CLOSE: u8 = 0xE0;
        /// A channel opened, the file's name following.
        pub const OPEN: u8 = 0xF0;
    }

    /// The channels LOAD and SAVE open on a serial device.
    pub mod channel {
        /// LOAD's, which reads a program or a disk's listing.
        pub const LOAD: u8 = 0;
        /// SAVE's, which writes a program.
        pub const SAVE: u8 = 1;
    }
}

/// The documented zero-page pointers a load leaves set.
pub mod zero_page {
    /// The start of BASIC's program text.
    pub const TXTTAB: u16 = 0x2B;
    /// The start of BASIC's variables, just past its text.
    pub const VARTAB: u16 = 0x2D;
    /// The start of BASIC's arrays.
    pub const ARYTAB: u16 = 0x2F;
    /// The end of BASIC's arrays.
    pub const STREND: u16 = 0x31;
    /// The KERNAL's address past the last byte loaded or saved.
    pub const LOAD_END: u16 = 0xAE;
}

/// CIA1's port B line the light pen input shares.
const LIGHT_PEN_LINE: u8 = 0x10;

/// CIA1's port A lines that switch control port 1's paddles, and port 2's,
/// to the SID.
const PADDLE_SELECT: [u8; 2] = [0x40, 0x80];

/// Drive cycles a drive runs before the other catches up, when there are
/// two: how late each sees what the other does to the bus.
const DRIVE_SLICE: u64 = 32;

/// The most cycles a CPU's step takes: a drive brought up to a cycle runs
/// on to the end of the step it was in.
const LONGEST_STEP: u64 = 7;

/// The sections a snapshot keeps each drive in, device 8's first.
const DRIVE_SECTIONS: [[u8; 4]; 2] = [*b"DRV8", *b"DRV9"];

/// One period of the 556 a press of RESTORE triggers, which holds NMI low:
/// 1.1 × 47 kΩ × 360 pF, in nanoseconds. Its trigger is coupled through a
/// capacitor, so holding the key on holds nothing.
const RESTORE_PULSE_NANOS: u64 = 18_612;

/// [`RESTORE_PULSE_NANOS`] in cycles of `standard`'s clock.
const fn restore_pulse(standard: Standard) -> u64 {
    (RESTORE_PULSE_NANOS * standard.clock()).div_ceil(1_000_000_000)
}

/// The VIC-II's view: its bank of memory, the character ROM where the PLA
/// puts it, and colour RAM.
struct View<'a> {
    memory: &'a Memory,
    bank: u8,
    /// The cartridge, in the Ultimax configuration: its ROMH answers the
    /// top 4 KiB of every bank, and the character ROM is gone.
    ultimax: Option<&'a Cartridge>,
}

impl VicMemory for View<'_> {
    fn fetch(&self, addr: u16) -> u8 {
        let Some(cartridge) = self.ultimax else {
            return self.memory.vic_read(self.bank, addr);
        };
        if addr & 0x3000 == 0x3000 {
            cartridge
                .read_romh(0x1000 | (addr & 0x0FFF))
                .unwrap_or(0xFF)
        } else {
            self.memory.ram(memory::vic_ram_address(self.bank, addr))
        }
    }

    fn colour(&self, addr: u16) -> u8 {
        self.memory.colour(addr)
    }
}

/// Everything but the CPU: memory, the chips, the keyboard, and time.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent line of the board"
)]
struct Board {
    now: u64,
    memory: Memory,
    vic: Vic,
    cia1: Cia,
    cia2: Cia,
    sid: Sid,
    /// The SID's sound since it was last taken, and how much is kept.
    audio: Vec<f32>,
    audio_capacity: usize,
    keyboard: Keyboard,
    /// The lines each joystick holds low, port 1's first.
    joysticks: [u8; 2],
    /// Each control port's paddles, X then Y.
    paddles: [[u8; 2]; 2],
    /// Devices 8 and 9, where fitted.
    drives: [Option<Drive>; 2],
    cartridge: Option<Cartridge>,
    datasette: Datasette,
    /// RESTORE, held, and the cycle its pulse lets NMI go.
    restore_held: bool,
    restore_until: u64,
    /// The light pen input as the VIC-II last saw it.
    light_pen: bool,
    irq: bool,
    nmi: bool,
    /// The first cycle a CIA must be brought up to.
    next_event: u64,
    monitor: Monitor,
}

impl Board {
    fn new(config: Config, roms: Roms<'_>) -> Result<Self, RomError> {
        let standard = config.standard;
        let mut board = Self {
            now: 0,
            memory: Memory::new(roms.basic, roms.kernal, roms.character)?,
            vic: Vic::new(standard.vic()),
            cia1: Cia::new(config.cia(), standard.mains()),
            cia2: Cia::new(config.cia(), standard.mains()),
            sid: Sid::new(config.sid, standard.clock()),
            audio: Vec::new(),
            audio_capacity: audio_capacity(standard),
            keyboard: Keyboard::new(),
            joysticks: [0; 2],
            paddles: [[0xFF; 2]; 2],
            drives: [None, None],
            cartridge: None,
            datasette: Datasette::new(standard.clock()),
            restore_held: false,
            restore_until: 0,
            light_pen: true,
            irq: false,
            nmi: false,
            next_event: u64::MAX,
            monitor: Monitor::default(),
        };
        board.follow_port();
        board.schedule();
        let bank = board.bank();
        board.vic.begin_cycle(&View {
            memory: &board.memory,
            bank,
            ultimax: None,
        });
        Ok(board)
    }

    /// The television standard the machine was built for.
    const fn standard(&self) -> Standard {
        match self.vic.model() {
            vic::Model::Mos6569 => Standard::Pal,
            vic::Model::Mos6567R8 => Standard::Ntsc,
        }
    }

    /// The VIC-II's bank: CIA2's port A lines 0 and 1, inverted.
    fn bank(&self) -> u8 {
        !self.cia2.port_a() & serial::BANK
    }

    fn schedule(&mut self) {
        let next = |cia: &Cia| cia.next_event().unwrap_or(u64::MAX);
        let tape = self.datasette.next_edge().unwrap_or(u64::MAX);
        self.next_event = next(&self.cia1).min(next(&self.cia2)).min(tape);
    }

    /// The datasette's motor and write lines brought to the port's levels,
    /// once the tape has played up to now.
    fn follow_port(&mut self) {
        self.play_tape();
        self.datasette.set_motor(self.now, self.memory.port.motor());
        self.datasette
            .set_write(self.now, self.memory.port.cassette_write());
    }

    /// Each pulse the tape has played by `now` a falling edge on FLAG.
    fn play_tape(&mut self) {
        while let Some(edge) = self.datasette.next_edge().filter(|&edge| edge <= self.now) {
            self.cia1.set_flag(edge, false);
            self.cia1.set_flag(edge, true);
            self.datasette.take_edge();
        }
    }

    /// End the cycle `bus` was carried in: the VIC-II's second half, the
    /// lines as the cycle leaves them, and the next cycle's first half.
    fn end_cycle(&mut self, bus: u8) {
        let bank = self.bank();
        let ultimax = self
            .cartridge
            .as_ref()
            .filter(|_| self.memory.cartridge.ultimax());
        self.vic.end_cycle(
            &View {
                memory: &self.memory,
                bank,
                ultimax,
            },
            bus,
        );
        self.now += 1;
        if self.now >= self.next_event {
            self.play_tape();
            self.cia1.advance(self.now);
            self.cia2.advance(self.now);
            self.schedule();
        }
        self.irq = self.vic.irq() || self.cia1.irq();
        self.nmi = self.cia2.irq() || self.now < self.restore_until;
        self.sync_cartridge();
        let ultimax = self
            .cartridge
            .as_ref()
            .filter(|_| self.memory.cartridge.ultimax());
        self.vic.begin_cycle(&View {
            memory: &self.memory,
            bank,
            ultimax,
        });
    }

    /// The expansion port's lines as the cartridge holds them now.
    fn sync_cartridge(&mut self) {
        self.memory.cartridge = self
            .cartridge
            .as_ref()
            .map_or(memory::Cartridge::NONE, |cartridge| {
                cartridge.lines(self.now)
            });
    }

    /// The CPU's read of `addr` in the current cycle, with whatever it
    /// sets off.
    fn access_read(&mut self, addr: u16) -> u8 {
        if addr <= 1 {
            return self
                .memory
                .port
                .read(addr, !self.datasette.sensed(), self.now);
        }
        let now = self.now;
        let floating = self.vic.phi1();
        let cartridge = self.cartridge.as_mut();
        match self.memory.cpu_chip(addr) {
            Chip::Io => self.io_read(addr),
            Chip::RomLow => cartridge
                .and_then(|c| c.read_roml(addr, now))
                .unwrap_or(floating),
            Chip::RomHigh => cartridge
                .and_then(|c| c.read_romh(addr))
                .unwrap_or(floating),
            Chip::Open => floating,
            chip @ (Chip::Ram | Chip::Basic | Chip::Kernal | Chip::Character) => {
                self.memory.read_chip(chip, addr)
            }
        }
    }

    /// What the data bus carries while a CPU read of `addr` waits on BA: the
    /// memory byte it addresses, or the floating bus where a chip or a
    /// cartridge answers, since the waiting read is not made until BA rises.
    fn waiting_bus(&self, addr: u16) -> u8 {
        let floating = self.vic.phi1();
        match self.memory.cpu_chip(addr) {
            _ if addr <= 1 => floating,
            Chip::Io if (io::COLOUR..io::CIA1).contains(&addr) => {
                self.memory.colour(addr) | floating & 0xF0
            }
            Chip::Io | Chip::RomLow | Chip::RomHigh | Chip::Open => floating,
            chip @ (Chip::Ram | Chip::Basic | Chip::Kernal | Chip::Character) => {
                self.memory.read_chip(chip, addr)
            }
        }
    }

    fn access_write(&mut self, addr: u16, value: u8) {
        if addr <= 1 {
            self.memory.port.write(addr, value, self.now);
            // The data bus still carries the VIC-II's byte to the RAM beneath.
            self.memory.write_ram(addr, self.vic.phi1());
            self.follow_port();
            self.schedule();
            return;
        }
        match self.memory.cpu_chip(addr) {
            Chip::Io => self.io_write(addr, value),
            // Writes under a cartridge's ROM reach RAM but in the Ultimax
            // configuration, where nothing inside answers.
            Chip::RomLow | Chip::RomHigh if !self.memory.cartridge.ultimax() => {
                self.memory.write_ram(addr, value);
            }
            Chip::Open | Chip::RomLow | Chip::RomHigh => {}
            Chip::Ram | Chip::Basic | Chip::Kernal | Chip::Character => {
                self.memory.write_ram(addr, value);
            }
        }
    }

    /// A read of the I/O area: a chip's register, colour RAM, or the
    /// expansion port's I/O, the bus monitor recording all but colour RAM.
    fn io_read(&mut self, addr: u16) -> u8 {
        let value = self.io_value(addr);
        if !matches!(addr, io::COLOUR..io::CIA1) {
            self.monitor.record(IoAccess {
                cycle: self.now,
                addr,
                value,
                write: false,
            });
        }
        value
    }

    fn io_value(&mut self, addr: u16) -> u8 {
        let register = addr.to_le_bytes()[0];
        match addr {
            io::VIC..io::SID => self.vic.read(register),
            io::SID..io::COLOUR => {
                self.sid
                    .read(self.now, register, &mut self.audio, self.audio_capacity)
            }
            io::COLOUR..io::CIA1 => self.memory.colour(addr) | self.vic.phi1() & 0xF0,
            io::CIA1..io::CIA2 => {
                let value = self.cia1.read(self.now, register);
                match register & 0x0F {
                    cia::reg::PRA => value & self.cia1_lines().0,
                    cia::reg::PRB => value & self.cia1_lines().1,
                    _ => value,
                }
            }
            io::CIA2..io::IO1 => {
                self.catch_up_drives();
                let value = self.cia2.read(self.now, register);
                if register & 0x0F == cia::reg::PRA {
                    value & self.serial_inputs()
                } else {
                    value
                }
            }
            _ => {
                let now = self.now;
                let floating = self.vic.phi1();
                self.cartridge
                    .as_mut()
                    .and_then(|cartridge| cartridge.read_io(addr, now))
                    .unwrap_or(floating)
            }
        }
    }

    fn io_write(&mut self, addr: u16, value: u8) {
        if !matches!(addr, io::COLOUR..io::CIA1) {
            self.monitor.record(IoAccess {
                cycle: self.now,
                addr,
                value,
                write: true,
            });
        }
        let register = addr.to_le_bytes()[0];
        match addr {
            io::VIC..io::SID => self.vic.write(register, value),
            io::SID..io::COLOUR => {
                self.sid.write(
                    self.now,
                    register,
                    value,
                    &mut self.audio,
                    self.audio_capacity,
                );
            }
            io::COLOUR..io::CIA1 => self.memory.write_colour(addr, value),
            io::CIA1..io::CIA2 => {
                self.cia1.write(self.now, register, value);
                self.follow_light_pen();
                self.route_paddles();
            }
            io::CIA2..io::IO1 => {
                self.catch_up_drives();
                self.cia2.write(self.now, register, value);
                self.tell_drives();
            }
            _ => {
                if let Some(cartridge) = self.cartridge.as_mut() {
                    cartridge.write_io(addr, value);
                }
            }
        }
        if matches!(addr, io::CIA1..io::IO1) {
            self.schedule();
        }
    }

    /// CIA1's port lines as the keyboard and the joysticks leave them.
    fn cia1_lines(&self) -> (u8, u8) {
        let [port1, port2] = self.joysticks;
        self.keyboard
            .resolve(self.cia1.port_a() & !port2, self.cia1.port_b() & !port1)
    }

    /// The serial bus lines the machine holds, each through an inverter
    /// from CIA2's port A.
    fn holds(&self) -> Lines {
        let driven = self.cia2.port_a();
        Lines {
            atn: driven & serial::ATN_OUT != 0,
            clock: driven & serial::CLOCK_OUT != 0,
            data: driven & serial::DATA_OUT != 0,
        }
    }

    /// The lines everything on the bus but drive `skip` holds.
    fn others(&self, skip: Option<usize>) -> Lines {
        self.drives
            .iter()
            .enumerate()
            .filter(|&(index, _)| Some(index) != skip)
            .filter_map(|(_, drive)| drive.as_ref())
            .fold(self.holds(), |lines, drive| lines.or(drive.holds()))
    }

    /// The serial bus's clock and data lines as CIA2 reads them, the rest of
    /// its port untouched: each low while anything on the bus holds it.
    fn serial_inputs(&self) -> u8 {
        let lines = self.others(None);
        let mut levels = !0;
        if lines.clock {
            levels &= !serial::CLOCK_IN;
        }
        if lines.data {
            levels &= !serial::DATA_IN;
        }
        levels
    }

    /// The drive cycle machine cycle `cycle` falls in.
    fn drive_cycle(&self, cycle: u64) -> u64 {
        let clock = self.standard().clock();
        let scaled = u128::from(cycle) * u128::from(drive::CLOCK) / u128::from(clock);
        u64::try_from(scaled).unwrap_or(u64::MAX)
    }

    /// Bring the drives up to the machine's time, two taking turns so each
    /// sees the other's lines.
    fn catch_up_drives(&mut self) {
        let target = self.drive_cycle(self.now);
        let fitted = self.drives.iter().flatten().count();
        loop {
            let mut behind = false;
            for index in 0..self.drives.len() {
                let others = self.others(Some(index));
                let Some(drive) = self.drives[index].as_mut() else {
                    continue;
                };
                let until = if fitted > 1 {
                    target.min(drive.cycles() + DRIVE_SLICE)
                } else {
                    target
                };
                drive.set_others(others);
                drive.run_until(until);
                behind |= drive.cycles() < target;
            }
            if !behind {
                return;
            }
        }
    }

    /// Give each drive the lines the rest of the bus now holds.
    fn tell_drives(&mut self) {
        for index in 0..self.drives.len() {
            let others = self.others(Some(index));
            if let Some(drive) = self.drives[index].as_mut() {
                drive.set_others(others);
            }
        }
    }

    /// Give the SID the paddles of the control port CIA1 switches to it;
    /// with neither or both switched, the lines pull the readings high.
    fn route_paddles(&mut self) {
        let lines = self.cia1.port_a();
        let switched: [bool; 2] = PADDLE_SELECT.map(|select| lines & select != 0);
        let [x, y] = match switched {
            [true, false] => self.paddles[0],
            [false, true] => self.paddles[1],
            [true, true] => [
                self.paddles[0][0] & self.paddles[1][0],
                self.paddles[0][1] & self.paddles[1][1],
            ],
            [false, false] => [0xFF; 2],
        };
        self.sid.set_pots(x, y);
    }

    /// Tell the VIC-II of any edge on the light pen input.
    fn follow_light_pen(&mut self) {
        let level = self.cia1_lines().1 & LIGHT_PEN_LINE != 0;
        if level != self.light_pen {
            self.light_pen = level;
            self.vic.set_light_pen(level);
        }
    }
}

/// Each paddle's channel: control port 1's X and Y, then port 2's.
pub const PADDLE_CHANNELS: usize = 4;

/// One second of the SID's sound at `standard`'s clock.
fn audio_capacity(standard: Standard) -> usize {
    let timing = standard.timing();
    usize::try_from(timing.clock() / timing.cycles_per_sample()).unwrap_or(usize::MAX)
}

impl Bus for Board {
    fn read(&mut self, addr: u16) -> u8 {
        while self.vic.ba() {
            let bus = if self.vic.dma() {
                self.vic.phi1()
            } else {
                self.waiting_bus(addr)
            };
            self.end_cycle(bus);
        }
        let value = self.access_read(addr);
        self.end_cycle(value);
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.access_write(addr, value);
        self.end_cycle(value);
    }

    fn irq(&self) -> bool {
        self.irq
    }

    fn nmi(&self) -> bool {
        self.nmi
    }
}

/// A Commodore 64.
pub struct Machine {
    cpu: Cpu,
    board: Board,
}

impl Machine {
    /// A machine built as `config` says with these ROMs, switched on.
    ///
    /// # Errors
    ///
    /// [`RomError::BadSize`] for an image not its ROM's size.
    pub fn new(config: Config, roms: Roms<'_>) -> Result<Self, RomError> {
        Ok(Self {
            cpu: Cpu::new(),
            board: Board::new(config, roms)?,
        })
    }

    /// Cycles since power-on.
    #[must_use]
    pub const fn cycles(&self) -> u64 {
        self.board.now
    }

    /// How it keeps time.
    #[must_use]
    pub const fn timing(&self) -> Timing {
        self.board.standard().timing()
    }

    /// Run to the first instruction boundary at or past `until`, the sound
    /// and the drives brought up to where it stops.
    pub fn run_until(&mut self, until: u64) {
        while self.board.now < until {
            self.cpu.step(&mut self.board);
        }
        let board = &mut self.board;
        board
            .sid
            .advance(board.now, &mut board.audio, board.audio_capacity);
        board.catch_up_drives();
    }

    /// Fit `unit` with a 1541 running `dos`, switched on with the machine:
    /// it runs from power-on to the machine's time, which costs as long as
    /// that, so a drive is fitted as the machine is built. Any drive it
    /// replaces goes with its disk.
    ///
    /// # Errors
    ///
    /// [`RomError::BadSize`] for a DOS not 16 KiB.
    pub fn fit_drive(&mut self, unit: Unit, dos: &[u8]) -> Result<(), RomError> {
        let mut drive = Drive::new(dos, unit)?;
        let board = &mut self.board;
        board.catch_up_drives();
        drive.run_until(board.drive_cycle(board.now));
        board.drives[unit.index()] = Some(drive);
        board.tell_drives();
        Ok(())
    }

    /// Take `unit`'s drive away, handing back its disk.
    pub fn remove_drive(&mut self, unit: Unit) -> Option<Surface> {
        let board = &mut self.board;
        board.catch_up_drives();
        let disk = board.drives[unit.index()]
            .take()
            .and_then(|mut drive| drive.eject());
        board.tell_drives();
        disk
    }

    /// RES: the CPU, the 6510's port, the CIAs, the SID, the cartridge and,
    /// through the serial bus, the drives start again; the VIC-II, with no
    /// reset line, runs on.
    pub fn reset(&mut self) {
        self.cpu.reset();
        let board = &mut self.board;
        let now = board.now;
        board.catch_up_drives();
        board.memory.port = memory::Port::new();
        board.follow_port();
        board.cia1.reset(now);
        board.cia2.reset(now);
        board.sid.reset(now, &mut board.audio, board.audio_capacity);
        if let Some(cartridge) = board.cartridge.as_mut() {
            cartridge.reset(now);
        }
        for drive in board.drives.iter_mut().flatten() {
            drive.reset();
        }
        board.sync_cartridge();
        board.tell_drives();
        board.route_paddles();
        board.follow_light_pen();
        board.schedule();
    }

    /// Plug in the cartridge `crt` holds, and reset, as a cartridge only
    /// goes in with the power off.
    ///
    /// # Errors
    ///
    /// The cartridge's [`CartridgeError`].
    pub fn insert_cartridge(&mut self, crt: &Crt) -> Result<(), CartridgeError> {
        self.plug_cartridge(Cartridge::new(crt)?);
        Ok(())
    }

    /// Plug in `cartridge`, and reset.
    pub fn plug_cartridge(&mut self, cartridge: Cartridge) {
        self.board.cartridge = Some(cartridge);
        self.reset();
    }

    /// Unplug the cartridge, handing it back, and reset.
    pub fn remove_cartridge(&mut self) -> Option<Cartridge> {
        let cartridge = self.board.cartridge.take();
        self.reset();
        cartridge
    }

    /// Whether a cartridge is plugged in.
    #[must_use]
    pub const fn holds_cartridge(&self) -> bool {
        self.board.cartridge.is_some()
    }

    /// Put `tape` in the datasette, handing back the one it replaces.
    pub fn insert_tape(&mut self, tape: Tape) -> Option<Tape> {
        let board = &mut self.board;
        board.play_tape();
        let old = board.datasette.insert(board.now, tape);
        board.schedule();
        old
    }

    /// Take the tape out.
    pub fn eject_tape(&mut self) -> Option<Tape> {
        let board = &mut self.board;
        board.play_tape();
        let old = board.datasette.eject(board.now);
        board.schedule();
        old
    }

    /// Press PLAY, with RECORD if `record`.
    pub fn press_play(&mut self, record: bool) {
        let board = &mut self.board;
        board.play_tape();
        board.datasette.press_play(board.now, record);
        board.schedule();
    }

    /// Press STOP.
    pub fn stop_tape(&mut self) {
        let board = &mut self.board;
        board.play_tape();
        board.datasette.stop(board.now);
        board.schedule();
    }

    /// Wind the tape to pulse `pulse`, the deck stopped.
    pub fn wind_tape(&mut self, pulse: usize) {
        let board = &mut self.board;
        board.datasette.wind(board.now, pulse);
        board.schedule();
    }

    /// The datasette.
    #[must_use]
    pub const fn datasette(&self) -> &Datasette {
        &self.board.datasette
    }

    /// The tape, with what has been recorded on it up to the machine's
    /// time.
    pub fn tape(&mut self) -> Option<&Tape> {
        let board = &mut self.board;
        board.play_tape();
        board.datasette.commit();
        board.datasette.tape()
    }

    /// `unit`'s drive, if fitted.
    #[must_use]
    pub fn drive(&self, unit: Unit) -> Option<&Drive> {
        self.board.drives[unit.index()].as_ref()
    }

    /// `unit`'s drive, if fitted, to change its disk.
    pub fn drive_mut(&mut self, unit: Unit) -> Option<&mut Drive> {
        self.board.catch_up_drives();
        self.board.drives[unit.index()].as_mut()
    }

    /// Keep the sound for the front end to take, or not.
    pub fn set_sound_output(&mut self, on: bool) {
        self.board.audio_capacity = if on {
            audio_capacity(self.board.standard())
        } else {
            0
        };
        if !on {
            self.board.audio.clear();
        }
    }

    /// The sound since last asked, a sample every [`sid::CYCLES_PER_SAMPLE`]
    /// cycles.
    pub fn take_audio(&mut self) -> Drain<'_, f32> {
        self.board.audio.drain(..)
    }

    /// Control port `port`'s paddles (0 for port 1) at `x` and `y`.
    pub fn set_paddles(&mut self, port: usize, x: u8, y: u8) {
        if let Some(paddles) = self.board.paddles.get_mut(port) {
            *paddles = [x, y];
            self.board.route_paddles();
        }
    }

    /// Press or release `key`.
    pub fn set_key(&mut self, key: Key, down: bool) {
        self.board.keyboard.set(key, down);
        self.board.follow_light_pen();
    }

    /// Press or release `keycap`: a key of the matrix, or RESTORE.
    pub fn press(&mut self, keycap: Keycap, down: bool) {
        match keycap {
            Keycap::Matrix(key) => self.set_key(key, down),
            Keycap::Restore => self.set_restore(down),
        }
    }

    /// Paddle `channel` of [`PADDLE_CHANNELS`] at `position`, from 0 to
    /// 255.
    pub fn set_paddle(&mut self, channel: usize, position: u8) {
        let board = &mut self.board;
        if let Some(paddle) = board.paddles.as_flattened_mut().get_mut(channel) {
            *paddle = position;
            board.route_paddles();
        }
    }

    /// Press or let go of RESTORE: a press pulls NMI low for one period of
    /// the 556 it triggers, which a press within that period leaves alone.
    pub fn set_restore(&mut self, down: bool) {
        let board = &mut self.board;
        if down && !board.restore_held && board.now >= board.restore_until {
            board.restore_until = board.now + restore_pulse(board.standard());
        }
        board.restore_held = down;
    }

    /// The lines of joystick `port` (0 for port 1) held, from
    /// [`joystick`].
    pub fn set_joystick(&mut self, port: usize, lines: u8) {
        if let Some(held) = self.board.joysticks.get_mut(port) {
            *held = lines & joystick::ALL;
            self.board.follow_light_pen();
        }
    }

    /// RAM at `addr`, beneath whatever the CPU sees there.
    #[must_use]
    pub fn ram(&self, addr: u16) -> u8 {
        self.board.memory.ram(addr)
    }

    /// How the letter keys type letters now: SHIFT makes capitals while the
    /// VIC-II shows the character ROM's lower-case set, and graphics in the
    /// upper-case set or any set of a program's own.
    #[must_use]
    pub fn letter_case(&self) -> LetterCase {
        let board = &self.board;
        let lower = !board.memory.cartridge.ultimax()
            && vic_character_offset(board.bank(), board.vic.character_base())
                == Some(LOWER_CASE_SET);
        if lower {
            LetterCase::Small
        } else {
            LetterCase::CapitalsOnly
        }
    }

    /// `program` put in RAM as LOAD leaves it: the KERNAL's end-of-load
    /// pointer past its last byte and, loaded at the start of BASIC's text,
    /// BASIC's variables and arrays beginning there.
    pub fn load_into_memory(&mut self, program: &Program) {
        let memory = &mut self.board.memory;
        let mut addr = program.address;
        for &byte in &program.bytes {
            memory.write_ram(addr, byte);
            addr = addr.wrapping_add(1);
        }
        let [low, high] = addr.to_le_bytes();
        let pointer = |memory: &mut Memory, at: u16| {
            memory.write_ram(at, low);
            memory.write_ram(at + 1, high);
        };
        pointer(memory, zero_page::LOAD_END);
        let text = u16::from_le_bytes([
            memory.ram(zero_page::TXTTAB),
            memory.ram(zero_page::TXTTAB + 1),
        ]);
        if program.address == text {
            for at in [zero_page::VARTAB, zero_page::ARYTAB, zero_page::STREND] {
                pointer(memory, at);
            }
        }
    }

    /// The CPU.
    #[must_use]
    pub const fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    /// Frames finished since power-on.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.board.vic.frames()
    }

    /// The last finished frame, a palette index a slot.
    #[must_use]
    pub fn shown(&self) -> &[u8] {
        self.board.vic.shown()
    }

    /// The rows of the finished frame that changed since last asked.
    pub fn take_dirty_rows(&mut self) -> DirtyRows {
        self.board.vic.take_dirty_rows()
    }

    /// Start or stop a bus monitor recording the CPU's accesses to the
    /// chips and the expansion port's I/O, colour RAM aside.
    pub fn monitor_io(&mut self, on: bool) {
        self.board.monitor.set(on);
    }

    /// The accesses the bus monitor has recorded since last asked.
    pub fn take_io_trace(&mut self) -> impl Iterator<Item = IoAccess> + '_ {
        self.board.monitor.take()
    }

    /// Every device brought up to the machine's time, as a snapshot holds
    /// them.
    fn settle(&mut self) {
        let board = &mut self.board;
        let now = board.now;
        board
            .sid
            .advance(now, &mut board.audio, board.audio_capacity);
        board.play_tape();
        board.cia1.advance(now);
        board.cia2.advance(now);
        board.schedule();
        board.catch_up_drives();
        for drive in board.drives.iter_mut().flatten() {
            drive.settle();
        }
    }

    /// A snapshot of the machine as it stands (`crate::snapshot`): every
    /// device, the drives with their disks, the tape and the cartridge, the
    /// ROMs by their fingerprints.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::TooLarge`] for a machine whose state outgrows a
    /// snapshot.
    pub fn snapshot(&mut self) -> Result<Vec<u8>, SnapshotError> {
        self.settle();
        let board = &self.board;
        let mut w = Writer::new();
        Model::C64.save(&mut w);
        w.section(*b"CONF", |w| {
            w.put(&board.standard());
            w.put(&board.sid.model());
        });
        w.section(*b"ROMS", |w| {
            board.memory.save_roms(w);
            for drive in &board.drives {
                w.put(&drive.as_ref().map(|drive| *drive.dos_fingerprint()));
            }
        });
        w.section(*b"TIME", |w| w.put(&board.now));
        w.section(*b"CPU ", |w| w.put(&self.cpu.state()));
        w.section(*b"BORD", |w| {
            board.memory.save_contents(w);
            w.put(&board.joysticks);
            w.put(&board.paddles);
            w.put(&board.restore_held);
            w.put(&board.restore_until);
            w.put(&board.light_pen);
        });
        w.section(*b"VIC ", |w| w.put(&board.vic));
        w.section(*b"CIA1", |w| w.put(&board.cia1));
        w.section(*b"CIA2", |w| w.put(&board.cia2));
        w.section(*b"SID ", |w| w.put(board.sid.state()));
        w.section(*b"KEYS", |w| w.put(&board.keyboard));
        w.section(*b"TAPE", |w| w.put(&board.datasette));
        w.section(*b"CART", |w| w.put(&board.cartridge));
        for (tag, drive) in DRIVE_SECTIONS.into_iter().zip(&board.drives) {
            if let Some(drive) = drive {
                w.section(tag, |w| drive.save(w));
            }
        }
        w.finish()
    }

    /// This machine's configuration, once the snapshot's is found to be it
    /// and its ROMs and drives' DOSes the ones fitted.
    fn check_fitting(&self, r: &mut Reader<'_>) -> Result<Config, SnapshotError> {
        let config = Config {
            standard: self.board.standard(),
            sid: self.board.sid.model(),
        };
        r.section(*b"CONF", |r| {
            let built = Config {
                standard: r.get()?,
                sid: r.get()?,
            };
            fitted(built == config)
        })?;
        r.section(*b"ROMS", |r| {
            self.board.memory.check_roms(r)?;
            for drive in &self.board.drives {
                let dos = r.get::<Option<[u8; FINGERPRINT_LEN]>>()?;
                fitted(dos.as_ref() == drive.as_ref().map(Drive::dos_fingerprint))?;
            }
            Ok(())
        })?;
        Ok(config)
    }

    /// Restore `snapshot`, taken on a machine built and fitted as this one
    /// is. The whole snapshot is checked before anything changes, so a
    /// refused one leaves the machine as it was; sound not yet taken is
    /// dropped, and the sound setting and bus monitors carry on.
    ///
    /// # Errors
    ///
    /// [`SnapshotError`]: the snapshot is damaged, holds what no machine
    /// can, or was taken on one built or fitted otherwise.
    pub fn restore(&mut self, snapshot: &[u8]) -> Result<(), SnapshotError> {
        let body = snapshot::open(snapshot)?;
        let mut r = Reader::new(&body);
        Model::C64.check(&mut r)?;
        let config = self.check_fitting(&mut r)?;
        let now = r.section(*b"TIME", Reader::get::<u64>)?;
        r.set_clock(now)?;
        let cpu = r.section(*b"CPU ", Reader::get::<CpuState>)?;
        let (memory, joysticks, paddles, (restore_held, restore_until), light_pen) =
            r.section(*b"BORD", |r| {
                let memory = self.board.memory.restored(r)?;
                let joysticks: [u8; 2] = r.get()?;
                let paddles = r.get()?;
                let restore = (r.get()?, r.get::<u64>()?);
                let light_pen = r.get()?;
                Reader::check(
                    joysticks.iter().all(|&held| held & !joystick::ALL == 0)
                        && restore.1.saturating_sub(now) <= restore_pulse(config.standard),
                )?;
                Ok((memory, joysticks, paddles, restore, light_pen))
            })?;
        let vic: Vic = r.section(*b"VIC ", Reader::get)?;
        let cia1: Cia = r.section(*b"CIA1", Reader::get)?;
        let cia2: Cia = r.section(*b"CIA2", Reader::get)?;
        let mut sid = self.board.sid.clone();
        sid.restore(r.section(*b"SID ", Reader::get::<SidState>)?)?;
        let keyboard = r.section(*b"KEYS", Reader::get)?;
        let datasette = r
            .section(*b"TAPE", Reader::get::<Datasette>)?
            .clocked(config.standard.clock());
        let cartridge = r.section(*b"CART", Reader::get)?;
        let target = self.board.drive_cycle(now);
        let mut drives = [None, None];
        for ((tag, fitted), restored) in DRIVE_SECTIONS
            .into_iter()
            .zip(&self.board.drives)
            .zip(&mut drives)
        {
            if let Some(drive) = fitted {
                let times = target..=target + LONGEST_STEP - 1;
                *restored = Some(r.section(tag, |r| drive.restored(r, times))?);
            }
        }
        let mains = config.standard.mains();
        Reader::check(
            r.left() == 0
                && datasette.motor() == memory.port.motor()
                && datasette.write_line() == memory.port.cassette_write()
                && vic.model() == config.standard.vic()
                && [&cia1, &cia2]
                    .iter()
                    .all(|cia| cia.model() == config.cia() && cia.mains() == mains),
        )?;
        let mut board = Board {
            now,
            memory,
            vic,
            cia1,
            cia2,
            sid,
            audio: Vec::new(),
            audio_capacity: self.board.audio_capacity,
            keyboard,
            joysticks,
            paddles,
            drives,
            cartridge,
            datasette,
            restore_held,
            restore_until,
            light_pen,
            irq: false,
            nmi: false,
            next_event: u64::MAX,
            monitor: self.board.monitor.fresh(),
        };
        board.sync_cartridge();
        board.irq = board.vic.irq() || board.cia1.irq();
        board.nmi = board.cia2.irq() || board.now < board.restore_until;
        board.schedule();
        board.tell_drives();
        self.cpu.restore(&cpu);
        self.board = board;
        Ok(())
    }

    /// CIA2's port A as the chip drives it: the VIC-II's bank, the serial
    /// bus's outputs and RS-232's TXD on the user port.
    #[cfg(test)]
    pub(crate) fn cia2_port_a(&self) -> u8 {
        self.board.cia2.port_a()
    }
}

/// Refuse a snapshot taken on a machine built or fitted otherwise.
const fn fitted(holds: bool) -> Result<(), SnapshotError> {
    if holds {
        Ok(())
    } else {
        Err(SnapshotError::Fitting)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
