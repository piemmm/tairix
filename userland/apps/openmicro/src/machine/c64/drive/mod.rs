//! The 1541: its own 6502 at 1 MHz, 2 KiB of RAM, the DOS in ROM, VIA1 on
//! the serial bus and VIA2 on the head. A drive runs behind the machine,
//! brought up to it whenever the machine touches the bus.
//!
//! The serial bus's three lines are open-collector, low while anything
//! holds them. The drive holds CLOCK and DATA through inverters from VIA1's
//! port B and reads all three through inverters; and it holds DATA too
//! while ATN and its acknowledge line disagree, so a drive answers ATN in
//! hardware before its DOS has looked.

pub mod head;

use alloc::boxed::Box;
use alloc::vec;

use tairix_crypto::FINGERPRINT_LEN;

use crate::cpu::{Bus, Cpu, CpuState};
use crate::machine::bbc::fdc::PhysicalDrive;
use crate::machine::via::{reg, FullRate, Via};
use crate::machine::{IoAccess, Monitor, Rom, RomError};
use crate::media::c1541::Surface;
use crate::snapshot::{self, Reader, SnapshotError, Writer};
use head::{Controls, Head, CRYSTAL_PER_CYCLE};

/// The drive's cycles a second.
pub const CLOCK: u64 = 1_000_000;

/// Bytes of the DOS ROM, at `$C000`.
pub const DOS_BYTES: usize = 0x4000;

/// Bytes of RAM, at `$0000`.
pub const RAM_BYTES: usize = 0x800;
/// VIA1, on the serial bus.
pub const VIA1: u16 = 0x1800;
/// VIA2, on the head.
pub const VIA2: u16 = 0x1C00;

/// VIA1's register `register`.
#[must_use]
pub const fn via1(register: u8) -> u16 {
    VIA1 + register as u16
}

/// VIA2's register `register`.
#[must_use]
pub const fn via2(register: u8) -> u16 {
    VIA2 + register as u16
}
/// The address lines the drive decodes below its ROM: RAM, open space and
/// the VIAs repeat every 8 KiB.
const DECODED: u16 = 0x1FFF;

/// Whether a read of `page` of the drive's address space acts on no device:
/// all but the VIAs' pages in each mirror.
#[must_use]
pub const fn quiet_read_page(page: u8) -> bool {
    page >= 0x80 || (page as u16) << 8 & DECODED < VIA1
}
/// Where the DOS ROM starts.
pub const DOS_BASE: u16 = 0xC000;

/// Drive cycles a disk going in or out blocks the write-protect sensor.
const INSERTION_CYCLES: u64 = CLOCK / 2;

/// The serial bus's lines a participant holds low.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lines {
    /// ATN, which only the machine holds.
    pub atn: bool,
    /// CLOCK.
    pub clock: bool,
    /// DATA.
    pub data: bool,
}

snapshot::fields!(Lines { atn, clock, data });

impl Lines {
    /// The lines either holds.
    #[must_use]
    pub const fn or(self, other: Self) -> Self {
        Self {
            atn: self.atn || other.atn,
            clock: self.clock || other.clock,
            data: self.data || other.data,
        }
    }
}

/// VIA1's port B lines: the bus read inverted, 1 while a line is held low,
/// and CLOCK and DATA driven low through inverters.
pub mod bus {
    /// DATA, read.
    pub const DATA_IN: u8 = 0x01;
    /// DATA, held low.
    pub const DATA_OUT: u8 = 0x02;
    /// CLOCK, read.
    pub const CLOCK_IN: u8 = 0x04;
    /// CLOCK, held low.
    pub const CLOCK_OUT: u8 = 0x08;
    /// ATNA: while it and ATN differ, the hardware holds DATA.
    pub const ATN_ACK: u8 = 0x10;
    /// ATN, read.
    pub const ATN_IN: u8 = 0x80;
    /// The output lines, which read high as inputs.
    pub const OUTPUTS: u8 = DATA_OUT | CLOCK_OUT | ATN_ACK;
    /// The device number's jumpers, the number less 8.
    pub const JUMPERS: u8 = 0x60;
}

/// VIA2's port B lines.
pub mod mechanism {
    /// The stepper's phase, which turns the head a half track for each
    /// step up or down.
    pub const PHASE: u8 = 0x03;
    /// The spindle motor, on.
    pub const MOTOR: u8 = 0x04;
    /// The activity lamp, lit.
    pub const LED: u8 = 0x08;
    /// High while the disk's write-protect notch is open.
    pub const WRITABLE: u8 = 0x10;
    /// The bit rate's zone, 3 for the outermost tracks.
    pub const ZONE: u8 = 0x60;
    /// Low while SYNC is seen.
    pub const SYNC: u8 = 0x80;
    /// The lines that read high as inputs.
    pub const PULLED_UP: u8 = 0x6F;
}

/// Everything but the CPU.
struct DriveBoard {
    now: u64,
    ram: Box<[u8]>,
    dos: Rom,
    via1: Via<FullRate>,
    via2: Via<FullRate>,
    head: Head,
    disk: Option<Surface>,
    /// The last byte on the data bus, which an unmapped read finds.
    bus: u8,
    /// The lines the machine and any other drive hold.
    others: Lines,
    /// Device number less 8, on the address jumpers.
    jumpers: u8,
    /// The write-protect sensor sees the disk again from this cycle.
    sensor_clear_at: u64,
    /// BYTE READY's level on VIA2's CA1, low in its pulse.
    byte_ready_line: bool,
    irq: bool,
    so: bool,
    next_event: u64,
    monitor: Monitor,
}

impl DriveBoard {
    /// The lines this drive holds with ATN at `atn`.
    fn holds(&self, atn: bool) -> Lines {
        let pins = self.via1.port_b();
        Lines {
            atn: false,
            clock: pins & bus::CLOCK_OUT != 0,
            data: pins & bus::DATA_OUT != 0 || atn != (pins & bus::ATN_ACK != 0),
        }
    }

    /// Give VIA1 the bus as everyone leaves it.
    fn follow_bus(&mut self) {
        let lines = self.others.or(self.holds(self.others.atn));
        let mut pins = bus::OUTPUTS | self.jumpers << bus::JUMPERS.trailing_zeros();
        for (held, bit) in [
            (lines.data, bus::DATA_IN),
            (lines.clock, bus::CLOCK_IN),
            (lines.atn, bus::ATN_IN),
        ] {
            if held {
                pins |= bit;
            }
        }
        self.via1.set_port_b_pins(self.now, pins);
        self.via1.set_ca1(self.now, lines.atn);
    }

    /// Give VIA2's port B the head's SYNC and the write-protect sensor.
    fn follow_mechanism(&mut self) {
        let mut pins = mechanism::PULLED_UP;
        if !self.head.sync() {
            pins |= mechanism::SYNC;
        }
        let writable = self.disk.as_ref().is_none_or(|disk| !disk.write_protected);
        if writable && self.now >= self.sensor_clear_at {
            pins |= mechanism::WRITABLE;
        }
        self.via2.set_port_b_pins(self.now, pins);
        self.via2.set_port_a_pins(self.head.latched());
    }

    /// Hand VIA2's outputs to the head.
    fn drive_head(&mut self) {
        let lines = self.via2.port_b();
        self.head.set_controls(Controls {
            motor: lines & mechanism::MOTOR != 0,
            phase: lines & mechanism::PHASE,
            zone: (lines & mechanism::ZONE) >> 5,
            reading: self.via2.cb2(),
            byte_ready: self.via2.ca2(),
            write_value: self.via2.port_a(),
        });
    }

    fn schedule(&mut self) {
        let next = |via: &Via<FullRate>| via.next_event().unwrap_or(u64::MAX);
        self.next_event = next(&self.via1).min(next(&self.via2));
    }

    fn end_cycle(&mut self) {
        let cycle = self.now;
        self.now += 1;
        self.head.advance(self.now, &mut self.disk);
        let ready = self.head.byte_ready(cycle);
        if ready == self.byte_ready_line {
            // The line falls with the pulse and rises after it.
            self.byte_ready_line = !ready;
            self.via2.set_port_a_pins(self.head.latched());
            self.via2.set_ca1(self.now, self.byte_ready_line);
            self.schedule();
        }
        if self.now >= self.next_event {
            self.via1.advance(self.now);
            self.via2.advance(self.now);
            self.schedule();
        }
        self.irq = self.via1.irq() || self.via2.irq();
        self.so = ready;
    }

    fn access_read(&mut self, addr: u16) -> u8 {
        if addr >= 0x8000 {
            return self.dos.image[usize::from(addr) & (DOS_BYTES - 1)];
        }
        let register = addr.to_le_bytes()[0] & 0x0F;
        match addr & DECODED {
            0x0000..=0x07FF => self.ram[usize::from(addr) & (RAM_BYTES - 1)],
            VIA1..=0x1BFF => {
                let value = self.via1.read(self.now, register);
                self.schedule();
                self.monitor.record(IoAccess {
                    cycle: self.now,
                    addr,
                    value,
                    write: false,
                });
                value
            }
            VIA2..=0x1FFF => {
                self.head.advance(self.now, &mut self.disk);
                self.follow_mechanism();
                let value = self.via2.read(self.now, register);
                self.schedule();
                self.monitor.record(IoAccess {
                    cycle: self.now,
                    addr,
                    value,
                    write: false,
                });
                value
            }
            _ => self.bus,
        }
    }

    fn access_write(&mut self, addr: u16, value: u8) {
        if addr >= 0x8000 {
            return;
        }
        let register = addr.to_le_bytes()[0] & 0x0F;
        if matches!(addr & DECODED, VIA1..=0x1FFF) {
            self.monitor.record(IoAccess {
                cycle: self.now,
                addr,
                value,
                write: true,
            });
        }
        match addr & DECODED {
            0x0000..=0x07FF => self.ram[usize::from(addr) & (RAM_BYTES - 1)] = value,
            VIA1..=0x1BFF => {
                self.via1.write(self.now, register, value);
                if matches!(register, reg::ORB | reg::DDRB) {
                    self.follow_bus();
                }
                self.schedule();
            }
            VIA2..=0x1FFF => {
                self.head.advance(self.now, &mut self.disk);
                self.via2.write(self.now, register, value);
                self.drive_head();
                self.follow_mechanism();
                self.schedule();
            }
            _ => {}
        }
    }
}

impl Bus for DriveBoard {
    fn read(&mut self, addr: u16) -> u8 {
        let value = self.access_read(addr);
        self.bus = value;
        self.end_cycle();
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.access_write(addr, value);
        self.bus = value;
        self.end_cycle();
    }

    fn irq(&self) -> bool {
        self.irq
    }

    fn nmi(&self) -> bool {
        false
    }

    fn so(&self) -> bool {
        self.so
    }
}

/// The device number a drive's jumpers set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Device 8.
    Eight,
    /// Device 9.
    Nine,
}

impl Unit {
    /// Both, device 8 first.
    pub const ALL: [Self; 2] = [Self::Eight, Self::Nine];

    /// The unit a machine's drive `drive` is: 8 for drive 0, 9 for drive 1.
    #[must_use]
    pub const fn of(drive: PhysicalDrive) -> Self {
        match drive {
            PhysicalDrive::Zero => Self::Eight,
            PhysicalDrive::One => Self::Nine,
        }
    }

    /// Its place in [`Unit::ALL`], where a machine keeps its drives.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Eight => 0,
            Self::Nine => 1,
        }
    }

    /// The jumpers' bits on VIA1's port B lines 5 and 6.
    const fn jumpers(self) -> u8 {
        match self {
            Self::Eight => 0,
            Self::Nine => 1,
        }
    }
}

/// A 1541.
pub struct Drive {
    cpu: Cpu,
    board: DriveBoard,
}

impl Drive {
    /// A drive answering as `unit` with `dos` in its ROM, switched on and
    /// empty.
    ///
    /// # Errors
    ///
    /// [`RomError::BadSize`] for a DOS not 16 KiB.
    pub fn new(dos: &[u8], unit: Unit) -> Result<Self, RomError> {
        if dos.len() != DOS_BYTES {
            return Err(RomError::BadSize);
        }
        let mut board = DriveBoard {
            now: 0,
            ram: vec![0; RAM_BYTES].into_boxed_slice(),
            dos: Rom::new(dos.into()),
            via1: Via::new(),
            via2: Via::new(),
            head: Head::new(),
            disk: None,
            bus: 0,
            others: Lines::default(),
            jumpers: unit.jumpers(),
            sensor_clear_at: 0,
            byte_ready_line: true,
            irq: false,
            so: false,
            next_event: u64::MAX,
            monitor: Monitor::default(),
        };
        board.follow_bus();
        board.follow_mechanism();
        Ok(Self {
            cpu: Cpu::new(),
            board,
        })
    }

    /// RES from the serial bus: the CPU and both VIAs start again, the head
    /// stopping where it stands.
    pub fn reset(&mut self) {
        self.cpu.reset();
        let board = &mut self.board;
        board.via1.reset(board.now);
        board.via2.reset(board.now);
        board.drive_head();
        board.follow_bus();
        board.follow_mechanism();
        board.schedule();
    }

    /// Drive cycles since power-on.
    #[must_use]
    pub const fn cycles(&self) -> u64 {
        self.board.now
    }

    /// The drive's CPU.
    #[must_use]
    pub const fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    /// Run to the first instruction boundary at or past drive cycle `until`.
    pub fn run_until(&mut self, until: u64) {
        while self.board.now < until {
            self.cpu.step(&mut self.board);
        }
    }

    /// The lines this drive holds, as the bus's ATN stands.
    #[must_use]
    pub fn holds(&self) -> Lines {
        self.board.holds(self.board.others.atn)
    }

    /// The machine and any other drive now hold `others`.
    pub fn set_others(&mut self, others: Lines) {
        if others != self.board.others {
            self.board.others = others;
            self.board.follow_bus();
            self.board.schedule();
        }
    }

    /// Put `disk` in, handing back the one it replaces; the disk's edge
    /// passes the write-protect sensor on the way.
    pub fn insert(&mut self, disk: Surface) -> Option<Surface> {
        let board = &mut self.board;
        board.head.advance(board.now, &mut board.disk);
        let old = board.disk.replace(disk);
        board.head.disk_changed();
        board.sensor_clear_at = board.now + INSERTION_CYCLES;
        old
    }

    /// Take the disk out.
    pub fn eject(&mut self) -> Option<Surface> {
        let board = &mut self.board;
        board.head.advance(board.now, &mut board.disk);
        let old = board.disk.take();
        if old.is_some() {
            board.head.disk_changed();
            board.sensor_clear_at = board.now + INSERTION_CYCLES;
        }
        old
    }

    /// The disk in the drive, if any.
    #[must_use]
    pub const fn disk(&self) -> Option<&Surface> {
        self.board.disk.as_ref()
    }

    /// The disk in the drive, to save.
    pub const fn disk_mut(&mut self) -> Option<&mut Surface> {
        self.board.disk.as_mut()
    }

    /// Whether the activity light is lit.
    #[must_use]
    pub fn led(&self) -> bool {
        self.board.via2.port_b() & mechanism::LED != 0
    }

    /// Whether the spindle motor runs.
    #[must_use]
    pub fn motor(&self) -> bool {
        self.board.via2.port_b() & mechanism::MOTOR != 0
    }

    /// The half-track under the head.
    #[must_use]
    pub const fn half_track(&self) -> u8 {
        self.board.head.half_track()
    }

    /// RAM at `addr`.
    #[must_use]
    pub fn ram(&self, addr: u16) -> u8 {
        self.board.ram[usize::from(addr) & (RAM_BYTES - 1)]
    }

    /// Start or stop a bus monitor recording the CPU's accesses to the
    /// VIAs, at drive cycles.
    pub fn monitor_io(&mut self, on: bool) {
        self.board.monitor.set(on);
    }

    /// The accesses the bus monitor has recorded since last asked.
    pub fn take_io_trace(&mut self) -> impl Iterator<Item = IoAccess> + '_ {
        self.board.monitor.take()
    }

    /// The fingerprint a snapshot records its DOS by.
    #[must_use]
    pub const fn dos_fingerprint(&self) -> &[u8; FINGERPRINT_LEN] {
        &self.board.dos.fingerprint
    }

    /// Both VIAs brought up to the drive's time, as a snapshot holds them.
    pub fn settle(&mut self) {
        let board = &mut self.board;
        board.via1.advance(board.now);
        board.via2.advance(board.now);
        board.schedule();
    }

    /// Record the drive's state, its disk with it, at its own time; settled
    /// first.
    pub fn save(&self, w: &mut Writer) {
        let board = &self.board;
        w.put(&board.now);
        w.put(&self.cpu.state());
        w.bytes(&board.ram);
        w.put(&board.via1);
        w.put(&board.via2);
        w.put(&board.head);
        w.put(&board.disk);
        w.put(&board.bus);
        w.put(&board.others);
        w.put(&board.sensor_clear_at);
        w.put(&board.byte_ready_line);
        w.put(&board.so);
    }

    /// This drive with a snapshot's state, whose time must fall within
    /// `times`, the drive cycles its machine's time allows. The reader's
    /// clock becomes the drive's, so it is to be one for this drive alone.
    ///
    /// # Errors
    ///
    /// [`SnapshotError`] for a state no drive holds.
    pub fn restored(
        &self,
        r: &mut Reader<'_>,
        times: core::ops::RangeInclusive<u64>,
    ) -> Result<Self, SnapshotError> {
        let now = r.get::<u64>()?;
        Reader::check(times.contains(&now))?;
        r.set_clock(now)?;
        let cpu = r.get::<CpuState>()?;
        let ram = r.bytes_at_most(RAM_BYTES)?;
        Reader::check(ram.len() == RAM_BYTES)?;
        let mut board = DriveBoard {
            now,
            ram: ram.into(),
            dos: self.board.dos.clone(),
            via1: r.get()?,
            via2: r.get()?,
            head: r.get()?,
            disk: r.get()?,
            bus: r.get()?,
            others: r.get()?,
            jumpers: self.board.jumpers,
            sensor_clear_at: r.get()?,
            byte_ready_line: r.get()?,
            irq: false,
            so: r.get()?,
            next_event: u64::MAX,
            monitor: self.board.monitor.fresh(),
        };
        Reader::check(
            board.head.now() == now * CRYSTAL_PER_CYCLE
                && board.sensor_clear_at <= now + INSERTION_CYCLES,
        )?;
        board.irq = board.via1.irq() || board.via2.irq();
        board.schedule();
        let mut restored = Self {
            cpu: Cpu::new(),
            board,
        };
        restored.cpu.restore(&cpu);
        Ok(restored)
    }

    /// A byte of RAM set, as a test's stand-in for a controller's command.
    #[cfg(test)]
    pub(crate) fn set_ram(&mut self, addr: u16, value: u8) {
        self.board.ram[usize::from(addr) & (RAM_BYTES - 1)] = value;
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
