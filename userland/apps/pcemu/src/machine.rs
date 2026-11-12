//! A whole machine: the CPU on its board, run against emulated time.
//!
//! [`Machine::run_until`] runs the CPU in slices that end where a device
//! next needs to act, so every interrupt is taken at the instruction it would
//! be on the hardware, and a halted CPU skips straight to the next event
//! without executing anything. Given translated code ([`Machine::attach`]),
//! the CPU runs it wherever a block is installed.

use crate::board::{
    Board, BoardConfig, ConfigError, EmsConfig, FixedAdapter, FloppyAdapter, ParallelPort,
    PortsConfig, SerialPort, SoundCard, SoundConfig,
};
use crate::chips::ppi::Wiring;
use crate::cpu::jit::dispatch::{self, Jit};
use crate::cpu::{Bus as _, Cause, Cpu, CpuModel, Stop};
use crate::debug::probe::Go;
use crate::ports::parallel::Device;
use crate::ports::serial::{Model, Plug};
use crate::snapshot::{check, In, Out, State, Torn};
use crate::storage::fdc::{DriveKind, Interface};
use crate::storage::hdc::Recording;
use crate::time::{CpuClock, Nanos, NANOS_PER_SECOND};
use crate::video::monitor::Monitor;
use crate::video::Adapter;

/// What a snapshot begins with, and the layout it is in. A build that
/// changes what any part keeps changes the version, and a snapshot of
/// another is refused rather than misread.
const SNAPSHOT_MAGIC: [u8; 4] = *b"PCSN";
const SNAPSHOT_VERSION: u16 = 2;

/// The most bytes a snapshot names its machine's build in.
pub const MAX_FINGERPRINT: usize = 1024;

/// The longest a machine a snapshot is taken of can have run: a century.
const MAX_AGE: Nanos = 100 * 365 * 86_400 * NANOS_PER_SECOND;

/// The audio stream rate a template's machine renders for; the front-end
/// sets its own device's.
pub const STREAM_RATE: u32 = 48_000;

/// The machines the application offers ready-made. Every field of the
/// configuration a template gives can be changed within what the hardware
/// it models allows. Each has a Microsoft mouse on COM1, a printer at
/// `378h`, a game port and a clock card, as the period's multifunction cards
/// gave a machine; the turbo and 80186 machines an 8087.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    /// The IBM PC (5150): an 8088 at 4.77 MHz and two 360 KiB drives.
    IbmPc,
    /// The IBM PC/XT (5160): an 8088 at 4.77 MHz, a 360 KiB drive, an MFM
    /// fixed disk and an AdLib.
    IbmXt,
    /// A turbo XT: a V20 at 10 MHz on an XT-class board, with a VGA, 1.44
    /// MiB and 1.2 MiB drives, an RLL fixed disk, a Sound Blaster 2.0, a
    /// second serial port and 2 MiB of expanded memory.
    TurboXt,
    /// An 80186 at 8 MHz on an XT-class board, with an EGA, a 1.44 MiB
    /// drive, an RLL fixed disk, a Sound Blaster 2.0, a second serial port
    /// and 8 MiB of expanded memory.
    Workstation186,
}

impl Template {
    /// The configuration the template stands for: 640 KiB, and the display
    /// adapter and drives the machine was sold with.
    #[must_use]
    pub fn config(self) -> BoardConfig {
        let (cpu, clock, wiring, adapter) = self.processor();
        let (floppy, fixed) = self.drives();
        BoardConfig {
            cpu,
            clock,
            wiring,
            memory_kib: 640,
            video: [Some(adapter), None],
            floppy,
            fixed,
            sound: SoundConfig {
                stream_rate: STREAM_RATE,
                card: self.sound_card(),
            },
            ports: self.ports(),
            ems: self.expanded(),
            coprocessor: matches!(self, Self::TurboXt | Self::Workstation186),
        }
    }

    /// The turbo XT's 2 MiB board and the 80186 machine's 8 MiB.
    fn expanded(self) -> Option<EmsConfig> {
        let pages = match self {
            Self::IbmPc | Self::IbmXt => return None,
            Self::TurboXt => 128,
            Self::Workstation186 => crate::ems::MAX_PAGES,
        };
        Some(expanded_board(pages))
    }

    fn processor(self) -> (CpuModel, CpuClock, Wiring, Adapter) {
        match self {
            Self::IbmPc => (
                CpuModel::I8088,
                CpuClock::PC,
                Wiring::Pc,
                Adapter::Cga(Monitor::Colour),
            ),
            Self::IbmXt => (
                CpuModel::I8088,
                CpuClock::PC,
                Wiring::Xt,
                Adapter::Cga(Monitor::Colour),
            ),
            Self::TurboXt => (
                CpuModel::V20,
                CpuClock::TURBO_XT,
                Wiring::Xt,
                Adapter::Vga(Monitor::Colour),
            ),
            Self::Workstation186 => (
                CpuModel::I80186,
                CpuClock::I80186_8,
                Wiring::Xt,
                Adapter::Ega(Monitor::Colour),
            ),
        }
    }

    fn drives(self) -> (Option<FloppyAdapter>, Option<FixedAdapter>) {
        let floppy = |interface, drives| Some(FloppyAdapter { interface, drives });
        let fixed = |recording| {
            Some(FixedAdapter {
                recording,
                jumpers: 0,
            })
        };
        let (dd, hd525, hd35) = (
            Some(DriveKind::Dd525),
            Some(DriveKind::Hd525),
            Some(DriveKind::Hd35),
        );
        match self {
            Self::IbmPc => (floppy(Interface::Standard, [dd, dd, None, None]), None),
            Self::IbmXt => (
                floppy(Interface::Standard, [dd, None, None, None]),
                fixed(Recording::Mfm),
            ),
            Self::TurboXt => (
                floppy(Interface::HighDensity, [hd35, hd525, None, None]),
                fixed(Recording::Rll),
            ),
            Self::Workstation186 => (
                floppy(Interface::HighDensity, [hd35, None, None, None]),
                fixed(Recording::Rll),
            ),
        }
    }

    /// The turbo and 80186 machines' sound cards share no line with their
    /// fixed disks' IRQ 5; the printer leaves IRQ 7 alone unless asked.
    fn sound_card(self) -> Option<SoundCard> {
        match self {
            Self::IbmPc => None,
            Self::IbmXt => Some(SoundCard::AdLib),
            Self::TurboXt | Self::Workstation186 => Some(SOUND_BLASTER),
        }
    }

    fn ports(self) -> PortsConfig {
        let (model, second) = match self {
            Self::IbmPc | Self::IbmXt => (Model::Ins8250, None),
            Self::TurboXt | Self::Workstation186 => (
                Model::Ns16450,
                Some(SerialPort {
                    model: Model::Ns16450,
                    device: Plug::Nothing,
                }),
            ),
        };
        let mouse = SerialPort {
            model,
            device: Plug::Mouse,
        };
        let printer = ParallelPort {
            base: 0x378,
            device: Device::Printer,
        };
        PortsConfig {
            serial: [Some(mouse), second, None, None],
            parallel: [Some(printer), None, None],
            game: true,
            clock: true,
        }
    }
}

/// The Sound Blaster as every machine fits it: at `220h` on IRQ 7, sharing
/// no line with a fixed disk's IRQ 5.
pub const SOUND_BLASTER: SoundCard = SoundCard::Blaster {
    base: 0x220,
    irq: 7,
};

/// An expanded memory board of `pages` as every machine fits it: its
/// registers at `258h`, its frame at `D000h`, clear of the adapters' ROMs.
#[must_use]
pub const fn expanded_board(pages: u16) -> EmsConfig {
    EmsConfig {
        port: 0x258,
        frame: 0xD_0000,
        pages,
    }
}

/// Why [`Machine::run_until`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Emulated time reached the deadline.
    Deadline,
    /// The emulator's ROM made host call `n`; the machine resumes after it.
    HostCall(u16),
    /// The debugger's probe stopped the processor; its reason is the
    /// board's ([`Board::take_stop`]).
    Break,
}

/// A machine.
#[derive(Debug)]
pub struct Machine {
    cpu: Cpu,
    board: Board,
    jit: Option<Jit>,
}

impl Machine {
    /// A machine as power-on leaves it.
    pub fn new(config: &BoardConfig) -> Result<Self, ConfigError> {
        Ok(Self {
            cpu: Cpu::new(config.cpu),
            board: Board::new(config)?,
            jit: None,
        })
    }

    /// The CPU.
    #[must_use]
    pub const fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    /// The CPU, to change.
    pub fn cpu_mut(&mut self) -> &mut Cpu {
        &mut self.cpu
    }

    /// The board.
    #[must_use]
    pub const fn board(&self) -> &Board {
        &self.board
    }

    /// The board, to change.
    pub fn board_mut(&mut self) -> &mut Board {
        &mut self.board
    }

    /// Runs `jit`'s blocks from now on, where they are installed.
    pub fn attach(&mut self, jit: Jit) {
        self.jit = Some(jit);
    }

    /// Takes back the translated code the CPU runs, emptied of everything
    /// translated from this machine's memory.
    pub fn detach(&mut self) -> Option<Jit> {
        let mut jit = self.jit.take()?;
        jit.flush(&mut self.board);
        Some(jit)
    }

    /// The translated code the CPU runs, if any, and the board it was
    /// translated from.
    pub fn blocks(&mut self) -> Option<(&mut Jit, &mut Board)> {
        Some((self.jit.as_mut()?, &mut self.board))
    }

    /// Sends the processor on from the stop the debugger's probe made, as
    /// `go` says.
    pub fn go(&mut self, go: Go) {
        self.board.go(go, &self.cpu);
    }

    /// Tells the bus the firmware enters interrupt `vector` for the program
    /// it runs, as the processor tells it of an INT.
    pub fn enters_interrupt(&mut self, vector: u8) {
        self.board.interrupts(vector, &self.cpu, Cause::Program);
    }

    /// The emulated time the CPU has reached.
    #[must_use]
    pub fn now(&self) -> Nanos {
        self.board.time_of(self.cpu.cycles())
    }

    /// Runs until emulated time reaches `deadline`, the ROM makes a host
    /// call, or the debugger's probe stops the processor.
    pub fn run_until(&mut self, deadline: Nanos) -> Exit {
        loop {
            let now = self.now();
            self.board.advance_to(now);
            if now >= deadline {
                return Exit::Deadline;
            }
            let next = self
                .board
                .next_event()
                .map_or(deadline, |event| event.min(deadline));
            let limit = self.board.cycles_at(next);
            let stop = match self.jit.as_mut() {
                Some(jit) => dispatch::run(&mut self.cpu, &mut self.board, limit, jit),
                None => self.cpu.run(&mut self.board, limit),
            };
            match stop {
                Stop::Limit | Stop::Rescheduled => {}
                Stop::Halted => {
                    if self.board.halt_stops() {
                        return Exit::Break;
                    }
                    self.cpu.idle_until(limit);
                    let held = self.board.idle_to(self.board.time_of(limit));
                    self.cpu.idle_until(limit.saturating_add(held));
                }
                Stop::HostCall(n) => {
                    self.board.advance_to(self.now());
                    return Exit::HostCall(n);
                }
                Stop::Break => {
                    self.board.advance_to(self.now());
                    return Exit::Break;
                }
            }
        }
    }

    /// The machine's whole state, for a machine built as `fingerprint` says;
    /// none where the heap will not hold it. Every device is first brought
    /// up to the processor's time, as it would be at the next instruction.
    ///
    /// The images in its drives are not in it: they are their owner's, given
    /// to the machine it is restored into.
    #[must_use]
    pub fn snapshot(&mut self, fingerprint: &[u8]) -> Option<alloc::vec::Vec<u8>> {
        let now = self.now();
        self.board.settle(now);
        let mut out = Out::refusable();
        out.raw(&SNAPSHOT_MAGIC);
        out.u16(SNAPSHOT_VERSION);
        out.bytes(fingerprint);
        self.cpu.save(&mut out);
        self.board.save(&mut out);
        out.finish()
    }

    /// Takes back a [`snapshot`](Self::snapshot) of a machine built as
    /// `fingerprint` says, into this one, built so too and not yet run.
    /// Every diskette drive is left empty, its disk-change line set where it
    /// held a disk.
    ///
    /// # Errors
    ///
    /// [`Torn::Mismatch`] for a snapshot of another build or another
    /// version, [`Torn::Malformed`] for one that does not read. The machine
    /// is then partly restored, and must be built again.
    pub fn restore(&mut self, bytes: &[u8], fingerprint: &[u8]) -> Result<(), Torn> {
        let mut input = In::new(bytes);
        if input.take(SNAPSHOT_MAGIC.len())? != SNAPSHOT_MAGIC
            || input.u16()? != SNAPSHOT_VERSION
            || input.bytes(MAX_FINGERPRINT)? != fingerprint
        {
            return Err(Torn::Mismatch);
        }
        self.cpu.restore(&mut input)?;
        self.board.restore(&mut input)?;
        if let Some(jit) = self.jit.as_mut() {
            jit.flush(&mut self.board);
        }
        check(input.is_exhausted())?;
        // The processor's time and the devices' agree, as a running machine
        // keeps them within a slice of each other, and both are of a machine
        // run for less than a lifetime.
        let (cpu, devices) = (self.now(), self.board.devices_now());
        check(cpu.abs_diff(devices) <= NANOS_PER_SECOND && devices < MAX_AGE)
    }

    /// Runs the CPU at `rate` from now on, as the turbo switch does. The
    /// devices keep their own clocks, so the guest's sense of time is
    /// unchanged.
    pub fn set_cpu_rate(&mut self, rate: CpuClock) {
        self.board.set_cpu_rate(rate, self.cpu.cycles());
    }
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "machine_snapshot_tests.rs"]
mod snapshot_tests;
