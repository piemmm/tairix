//! The system board: memory, the I/O decode, the chips, the disk adapters,
//! the sound sources, the serial, parallel and game ports and the clock
//! card, interrupt routing, DRAM refresh, and the emulated time they all
//! keep.
//!
//! Devices are lazy. The board catches them up to the CPU's cycle count
//! whenever the CPU touches one, and reports the next instant one needs to
//! act on its own (a timer edge, a keystroke's byte, an 80186 timer, a byte a
//! disk moves over DMA), so the CPU runs uninterrupted up to it. Every DMA
//! transfer holds the CPU off the bus for its cycle.

mod blocks;
pub mod memory;
mod probing;
mod snapshot;

use crate::bits::word;
use crate::chips::dma::{Dma, DmaMemory};
use crate::chips::keyboard::{Keyboard, Scan};
use crate::chips::pcb186::{Ack, DmaBus, Effect, Pcb, Space};
use crate::chips::pic::Pic;
use crate::chips::pit::Pit;
use crate::chips::ppi::{port_b, BootDisplay, Ppi, Switches, Wiring};
use crate::cpu::bus::Escape;
use crate::cpu::jit::page::PageMap;
use crate::cpu::{Bus, Cause, Cpu, CpuModel, Family, BUS_CYCLE, OPEN_BUS};
use crate::debug::probe::{Cycle, Master, Probe};
use crate::ems::{self, Ems};
use crate::folder::{Share, SHARES};
use crate::fpu::{self, Fpu};
use crate::ports::clock::{self, ClockCard};
use crate::ports::game::{self, GamePort, Sticks};
use crate::ports::mouse::HostPointer;
use crate::ports::parallel::{self, Device, Parallel};
use crate::ports::serial::{self, Model, Plug, Uart};
use crate::sound::blaster::Dsp;
use crate::sound::opl2::Opl2;
use crate::sound::speaker::Speaker;
use crate::sound::{MixError, Mixer, Source, TICKS_PER_SAMPLE};
use crate::storage::fdc::{self, DriveKind, Fdc, Interface};
use crate::storage::floppy::Floppy;
use crate::storage::hdc::{self, AttachError, DiskType, Hdc, LowLevel, Recording};
use crate::storage::BlockDevice;
use crate::time::{Clock, CpuClock, Hertz, Nanos};
use crate::video::{Adapter, Card, Frame, ShownText, Unread};
use alloc::boxed::Box;
use alloc::vec::Vec;
use memory::{MapError, Memory};
use tairix_abi::time::CivilTime;

/// What answers a port on the system board's decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Port {
    None,
    Dma,
    Pic,
    Pit,
    Ppi,
    DmaPage,
    NmiMask,
    /// A display adapter, by its number.
    Video(u8),
    Floppy,
    FixedDisk,
    /// The YM3812, at the AdLib's ports or the Sound Blaster's.
    Fm,
    Blaster,
    /// A serial adapter, COM1 being 0.
    Serial(u8),
    /// A parallel adapter, by its place in the configuration.
    Parallel(u8),
    Game,
    Clock,
    /// The expanded memory board's page registers.
    Ems,
}

/// The YM3812's ports on an AdLib, which the Sound Blaster also answers.
const ADLIB: u16 = 0x388;
/// How often the board renders its sound with every source caught up, so
/// the mix holds little and no source's change arrives after its samples.
const RENDER_PERIOD: Nanos = 10_000_000;

/// The ports a PC/XT-class board decodes: ten address bits, so every port
/// repeats each kilobyte.
const PORTS: usize = 1024;
/// The board holds every I/O cycle for one wait state.
const IO_WAIT_STATES: u8 = 1;
/// The board's memory answers without wait states.
const MEMORY_WAIT_STATES: u8 = 0;
/// A DMA cycle takes the bus for four clocks and the board's wait state.
const DMA_CYCLES: u64 = 5;
/// Kilobytes in the address space, the wait table's granularity.
const KIBS: usize = 1024;
/// Marks a kilobyte of the wait table whose wait states change within it.
const MIXED: u8 = 0xFF;
/// The timer channel that requests refresh, and the DMA channel it requests.
const REFRESH_TIMER: usize = 1;
const REFRESH_DMA: usize = 0;
/// The clocks the 8087's request/grant exchange takes the bus for around
/// its transfers: the request, the grant, the release and the dead clock
/// after it.
const EXCHANGE_CYCLES: u64 = 4;

/// The machine a board is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardConfig {
    /// The processor, which brings the 80186's peripherals with it.
    pub cpu: CpuModel,
    /// The processor's clock.
    pub clock: CpuClock,
    /// Which IBM board's 8255 wiring.
    pub wiring: Wiring,
    /// Conventional memory in KiB: 64 to 640, in 64 KiB steps.
    pub memory_kib: u16,
    /// The display adapters: at most one monochrome and one colour, as the
    /// period's dual-monitor machines had.
    pub video: [Option<Adapter>; 2],
    /// The floppy adapter, if fitted.
    pub floppy: Option<FloppyAdapter>,
    /// The fixed-disk adapter, if fitted. Its drives attach once the board
    /// is built.
    pub fixed: Option<FixedAdapter>,
    /// The sound card, and the stream the mix plays to.
    pub sound: SoundConfig,
    /// The serial, parallel and game ports, and the clock card.
    pub ports: PortsConfig,
    /// The expanded memory board, if fitted.
    pub ems: Option<EmsConfig>,
    /// Whether an 8087 sits in the coprocessor socket.
    pub coprocessor: bool,
}

/// An expanded memory board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmsConfig {
    /// Its registers' base: one of [`ems::BASES`].
    pub port: u16,
    /// Its frame: one of [`ems::FRAMES`].
    pub frame: u32,
    /// Its pages of 16 KiB, from 1 to [`ems::MAX_PAGES`].
    pub pages: u16,
}

/// The sound a board makes, beyond its speaker and what a parallel port
/// carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoundConfig {
    /// The rate of the audio stream the mix is carried to, in hertz.
    pub stream_rate: u32,
    /// The sound card, if fitted.
    pub card: Option<SoundCard>,
}

/// The ports a board's adapters bring, and the clock card. An interrupt
/// line two of them share is high while either drives it, as ISA's
/// tri-stated adapters share one when only one drives it at a time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortsConfig {
    /// COM1 to COM4, at the bases and interrupts IBM gave them.
    pub serial: [Option<SerialPort>; 4],
    /// The parallel adapters, each at its own base.
    pub parallel: [Option<ParallelPort>; 3],
    /// A game port at `201h`.
    pub game: bool,
    /// The clock card at `2C0h`.
    pub clock: bool,
}

/// A serial adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerialPort {
    /// Its UART.
    pub model: Model,
    /// What is plugged into it.
    pub device: Plug,
}

/// A sound card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundCard {
    /// The AdLib: a YM3812 at `388h`.
    AdLib,
    /// The Sound Blaster 2.0 at `base` (`220h` or `240h`), on DMA channel 1
    /// and IRQ `irq` (2, 3, 5 or 7), its YM3812 at its own ports and the
    /// AdLib's.
    Blaster {
        /// Its port base.
        base: u16,
        /// Its interrupt.
        irq: u8,
    },
}

/// A parallel adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParallelPort {
    /// Its base: one of [`parallel::BASES`].
    pub base: u16,
    /// What is plugged into it.
    pub device: Device,
}

/// A floppy adapter and its drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloppyAdapter {
    /// Which adapter.
    pub interface: Interface,
    /// The drives on its cable.
    pub drives: [Option<DriveKind>; 4],
}

/// A fixed-disk adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedAdapter {
    /// How its controller records a track.
    pub recording: Recording,
    /// Its drive-type jumpers.
    pub jumpers: u8,
}

/// What the drives' lamps show, a bit for each drive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lights {
    /// Diskette drives whose motors run.
    pub floppy: u8,
    /// Fixed disks a command is under way on.
    pub fixed: u8,
}

/// Why a board could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Memory outside 64–640 KiB or not a multiple of 64 KiB.
    Memory,
    /// The memory could not be allocated.
    OutOfMemory,
    /// Two adapters answer the same ports or memory.
    VideoConflict,
    /// The audio stream's rate is zero.
    StreamRate,
    /// A sound card at a base or interrupt it cannot be set to, or one
    /// another device uses.
    SoundConflict,
    /// A port or the clock card at a base it cannot be set to, or one
    /// another device uses.
    PortConflict,
    /// An expanded memory board of a size or frame it cannot have.
    Expanded,
}

/// The system board's own bus: memory and the planar chips. DMA transfers
/// reach it without the CPU.
#[derive(Debug)]
struct Planar {
    memory: Memory,
    /// What answers each port.
    decode: [Port; PORTS],
    pic: Pic,
    pit: Pit,
    dma: Dma,
    ppi: Ppi,
    keyboard: Keyboard,
    cards: Vec<Card>,
    fdc: Option<Fdc>,
    hdc: Option<Hdc>,
    sound: Sound,
    ports: Ports,
    ems: Option<Expanded>,
    /// When the sound was last rendered.
    rendered: Nanos,
    now: Nanos,
    pit_tick: u64,
    /// The timer tick through which refresh has been accounted.
    refresh_tick: u64,
    /// Port `A0h`'s bit 7: NMI may reach the CPU.
    nmi_enabled: bool,
    /// The debugger's probe, while one is on the board.
    probe: Option<Box<Probe>>,
}

/// The sound sources and their mix.
#[derive(Debug)]
struct Sound {
    mixer: Mixer,
    speaker: Speaker,
    fm: Option<Opl2>,
    blaster: Option<Blaster>,
}

/// The expanded memory board, where it sits, and where its pages begin in
/// the store.
#[derive(Debug)]
struct Expanded {
    board: Ems,
    port: u16,
    frame: u32,
    store: u32,
}

impl Expanded {
    /// Shows in `memory` what window `n` now shows.
    fn show(&self, n: usize, memory: &mut Memory) {
        let Ok(window) = u32::try_from(n) else {
            return;
        };
        let offset = self
            .board
            .shown(n)
            .map(|page| self.store + u32::from(page) * ems::PAGE);
        memory.show_banked(self.frame + window * ems::PAGE, ems::PAGE, offset);
    }
}

/// The serial, parallel and game ports, and the clock card.
#[derive(Debug)]
struct Ports {
    serial: [Option<Uart>; 4],
    parallel: [Option<(Parallel, u16)>; 3],
    game: Option<GamePort>,
    clock: Option<ClockCard>,
}

impl Ports {
    fn new(config: &PortsConfig) -> Self {
        Self {
            serial: config
                .serial
                .map(|port| port.map(|port| Uart::new(port.model, port.device))),
            parallel: config
                .parallel
                .map(|port| port.map(|port| (Parallel::new(port.device), port.base))),
            game: config.game.then(GamePort::default),
            clock: config.clock.then(|| ClockCard::new(0)),
        }
    }
}

/// A Sound Blaster's DSP and where it is jumpered.
#[derive(Debug)]
struct Blaster {
    dsp: Dsp,
    base: u16,
    irq: u8,
}

impl Sound {
    fn new(config: SoundConfig) -> Result<Self, ConfigError> {
        let mixer = Mixer::new(config.stream_rate).map_err(|error| match error {
            MixError::Rate => ConfigError::StreamRate,
            MixError::OutOfMemory => ConfigError::OutOfMemory,
        })?;
        let (fm, blaster) = match config.card {
            None => (None, None),
            Some(SoundCard::AdLib) => (Some(Opl2::new()), None),
            Some(SoundCard::Blaster { base, irq }) => (
                Some(Opl2::new()),
                Some(Blaster {
                    dsp: Dsp::new(),
                    base,
                    irq,
                }),
            ),
        };
        Ok(Self {
            mixer,
            speaker: Speaker::new(),
            fm,
            blaster,
        })
    }

    /// Makes the YM3812's samples begun by `now`.
    fn run_fm(&mut self, now: Nanos) {
        let Some(fm) = self.fm.as_mut() else {
            return;
        };
        let until = Hertz::CRYSTAL.ticks_at(now) / TICKS_PER_SAMPLE + 1;
        let mixer = &mut self.mixer;
        fm.run_to(until, &mut |n, sample| {
            mixer.sample(Source::Fm, n, f32::from(sample) / 32_768.0);
        });
    }
}

/// Memory as a bus master other than the CPU reaches it: RAM and ROM, and
/// the cards' windows. The 80186's control block is inside the CPU, so no
/// such transfer sees it.
struct BusMemory<'a> {
    memory: &'a mut Memory,
    cards: &'a mut [Card],
    now: Nanos,
    probe: Option<&'a mut Probe>,
}

impl BusMemory<'_> {
    fn watch(&mut self, addr: u32, value: u8, cycle: Cycle) {
        let probed = self.memory.probed(addr);
        if let Some(probe) = self.probe.as_deref_mut().filter(|_| probed) {
            probe.memory(addr, value, cycle, Master::Dma);
        }
    }
}

impl DmaMemory for BusMemory<'_> {
    fn dma_read(&mut self, addr: u32) -> u8 {
        let value = match self.memory.device(addr) {
            Some(n) => self
                .cards
                .get_mut(n)
                .map_or(OPEN_BUS, |card| card.read(addr)),
            None => self.memory.read(addr),
        };
        self.watch(addr, value, Cycle::Read);
        value
    }

    fn dma_write(&mut self, addr: u32, value: u8) {
        match self.memory.device(addr) {
            Some(n) => {
                if let Some(card) = self.cards.get_mut(n) {
                    card.write(addr, value, self.now);
                }
            }
            None => self.memory.write(addr, value),
        }
        self.watch(addr, value, Cycle::Write);
    }
}

impl Planar {
    fn bus_memory(&mut self) -> BusMemory<'_> {
        BusMemory {
            memory: &mut self.memory,
            cards: &mut self.cards,
            now: self.now,
            probe: self.probe.as_deref_mut(),
        }
    }

    /// Catches the Sound Blaster's DSP up to the planar's time; its caller
    /// drives the lines after.
    fn advance_sound(&mut self) {
        let now = self.now;
        let Some(blaster) = self.sound.blaster.as_mut() else {
            return;
        };
        let mut bus = BusMemory {
            memory: &mut self.memory,
            cards: &mut self.cards,
            now,
            probe: self.probe.as_deref_mut(),
        };
        blaster
            .dsp
            .advance(now, &mut self.dma, &mut bus, &mut self.sound.mixer);
    }

    /// Catches the disk adapters up to the planar's time; its caller drives
    /// the lines after.
    fn advance_storage(&mut self) {
        let now = self.now;
        let mut bus = BusMemory {
            memory: &mut self.memory,
            cards: &mut self.cards,
            now,
            probe: self.probe.as_deref_mut(),
        };
        if let Some(fdc) = self.fdc.as_mut() {
            fdc.advance(now, &mut self.dma, &mut bus);
        }
        if let Some(hdc) = self.hdc.as_mut() {
            hdc.advance(now, &mut self.dma, &mut bus);
        }
    }

    /// Drives interrupt lines 2 to 7 from every adapter on them.
    fn sync_lines(&mut self) {
        let mut lines = 0u8;
        let mut drive = |irq: u8, high: bool| lines |= u8::from(high) << (irq & 7);
        drive(2, self.cards.iter().any(Card::irq));
        if let Some(fdc) = self.fdc.as_ref() {
            drive(fdc::IRQ, fdc.irq());
        }
        if let Some(hdc) = self.hdc.as_ref() {
            drive(hdc::IRQ, hdc.irq());
        }
        if let Some(blaster) = self.sound.blaster.as_ref() {
            drive(blaster.irq, blaster.dsp.irq());
        }
        for (uart, irq) in self.ports.serial.iter().zip(serial::IRQS) {
            if let Some(uart) = uart {
                drive(irq, uart.irq());
            }
        }
        for (lpt, base) in self.ports.parallel.iter().flatten() {
            drive(parallel::irq(*base), lpt.irq(self.now));
        }
        for irq in 2..8 {
            self.pic.set_line(irq, lines & (1 << irq) != 0);
        }
    }

    fn pit_tick_at(t: Nanos) -> u64 {
        Hertz::PIT.ticks_at(t)
    }

    /// Catches the chips up to `t`, acting on everything due by then even
    /// when no time has passed.
    fn advance_to(&mut self, t: Nanos) {
        // Work a device was handed at the planar's own time, which can lead
        // the CPU's by part of a cycle, is due whatever time is asked for:
        // left waiting, its event would hold the CPU's limit in the past.
        let t = t.max(self.now);
        self.now = t;
        let tick = Self::pit_tick_at(t);
        if self
            .pit
            .next_rise(0, self.pit_tick)
            .is_some_and(|rise| rise <= tick)
        {
            self.pic.rising_edge(0);
        }
        self.pic.set_line(0, self.pit.out(0, tick));
        self.pit_tick = tick;
        self.keyboard.advance(t);
        self.pic.set_line(1, self.keyboard.irq());
        for card in &mut self.cards {
            // A card whose interrupt is due renders up to it; the rest wait
            // to be looked at.
            if card.irq() || card.next_interrupt().is_some_and(|at| at <= t) {
                card.catch_up(t);
            }
        }
        for uart in self.ports.serial.iter_mut().flatten() {
            uart.advance(t);
        }
        self.advance_storage();
        if t >= self.rendered + RENDER_PERIOD {
            self.render_audio();
        } else {
            self.advance_sound();
            self.sync_lines();
        }
    }

    /// Runs the refresh cycles timer 1 requested through `tick`; returns how
    /// many took the bus.
    fn refresh_until(&mut self, tick: u64) -> u64 {
        refresh_until(&self.pit, &mut self.dma, &mut self.refresh_tick, tick)
    }

    /// When the planar next acts on its own, if its acting could be seen: an
    /// interrupt it could raise, or a byte a disk moves.
    fn next_event(&self) -> Option<Nanos> {
        let timer = self
            .pic
            .listens_to(0)
            .then(|| {
                self.pit
                    .next_rise(0, self.pit_tick)
                    .map(|rise| Hertz::PIT.time_of(rise))
            })
            .flatten();
        let keyboard = self
            .pic
            .listens_to(1)
            .then(|| self.keyboard.next_event())
            .flatten();
        let video = self
            .pic
            .listens_to(2)
            .then(|| self.cards.iter().filter_map(Card::next_interrupt).min())
            .flatten();
        let floppy = self.fdc.as_ref().and_then(Fdc::next_event);
        let fixed = self.hdc.as_ref().and_then(Hdc::next_event);
        let dsp = self.sound.blaster.as_ref().and_then(|b| b.dsp.next_event());
        let serial = self
            .ports
            .serial
            .iter()
            .zip(serial::IRQS)
            .filter(|&(_, irq)| self.pic.listens_to(irq))
            .filter_map(|(uart, _)| uart.as_ref()?.next_event())
            .min();
        let printer = self
            .ports
            .parallel
            .iter()
            .flatten()
            .filter(|(_, base)| self.pic.listens_to(parallel::irq(*base)))
            .filter_map(|(lpt, _)| lpt.next_event(self.now))
            .min();
        earliest(
            earliest(earliest(timer, keyboard), video),
            earliest(
                earliest(floppy, fixed),
                earliest(dsp, earliest(serial, printer)),
            ),
        )
    }

    fn input(&mut self, port: u16) -> u8 {
        let low = crate::bits::lo(port);
        match self.decode[usize::from(port) % PORTS] {
            Port::Dma => self.dma.read(low),
            Port::Pic => self.pic.read(port & 1 != 0),
            Port::Pit => self.pit.read(low, self.pit_tick),
            Port::Ppi => match low & 3 {
                0 => self.ppi.read_a(self.keyboard.data()),
                1 => self.ppi.read_b(),
                2 => self.ppi.read_c(self.pit.out(2, self.pit_tick)),
                _ => OPEN_BUS,
            },
            Port::Video(n) => self
                .cards
                .get_mut(usize::from(n))
                .map_or(OPEN_BUS, |card| card.input(port, self.now)),
            Port::Floppy | Port::FixedDisk => self.disk_input(port).unwrap_or(OPEN_BUS),
            Port::Fm => self.fm_input(port),
            Port::Blaster => self.blaster_input(port),
            Port::Serial(n) => self.serial_input(n, port),
            Port::Parallel(n) => self.parallel_input(n, port),
            Port::Game => self
                .ports
                .game
                .map_or(OPEN_BUS, |game| game.input(self.now)),
            Port::Clock => {
                let now = self.now;
                self.ports.clock.as_mut().map_or(OPEN_BUS, |card| {
                    card.input(port.wrapping_sub(clock::PORT) % clock::PORTS, now)
                })
            }
            Port::Ems => self.ems.as_ref().map_or(OPEN_BUS, |ems| {
                ems.board.input(port.wrapping_sub(ems.port) % ems::PORTS)
            }),
            Port::DmaPage | Port::NmiMask | Port::None => OPEN_BUS,
        }
    }

    /// The YM3812's status, at the address port; its data port reads
    /// nothing.
    fn fm_input(&mut self, port: u16) -> u8 {
        self.sound.run_fm(self.now);
        match self.sound.fm.as_ref() {
            Some(fm) if port & 1 == 0 => fm.status(),
            _ => OPEN_BUS,
        }
    }

    fn fm_output(&mut self, port: u16, value: u8) {
        self.sound.run_fm(self.now);
        if let Some(fm) = self.sound.fm.as_mut() {
            if port & 1 == 0 {
                fm.select(value);
            } else {
                fm.write(value);
            }
        }
    }

    fn blaster_input(&mut self, port: u16) -> u8 {
        self.advance_sound();
        let Some(blaster) = self.sound.blaster.as_mut() else {
            return OPEN_BUS;
        };
        let value = blaster.dsp.input(port.wrapping_sub(blaster.base) & 0x0F);
        self.sync_lines();
        value
    }

    fn blaster_output(&mut self, port: u16, value: u8) {
        self.advance_sound();
        if let Some(blaster) = self.sound.blaster.as_mut() {
            blaster
                .dsp
                .output(port.wrapping_sub(blaster.base) & 0x0F, value, self.now);
        }
        self.advance_sound();
        self.sync_lines();
    }

    fn serial_input(&mut self, n: u8, port: u16) -> u8 {
        let now = self.now;
        let n = usize::from(n);
        let value = match (self.ports.serial.get_mut(n), serial::BASES.get(n)) {
            (Some(Some(uart)), Some(&base)) => uart.input(port.wrapping_sub(base) & 7, now),
            _ => OPEN_BUS,
        };
        self.sync_lines();
        value
    }

    fn serial_output(&mut self, n: u8, port: u16, value: u8) {
        let now = self.now;
        let n = usize::from(n);
        if let (Some(Some(uart)), Some(&base)) =
            (self.ports.serial.get_mut(n), serial::BASES.get(n))
        {
            uart.output(port.wrapping_sub(base) & 7, value, now);
        }
        self.sync_lines();
    }

    fn parallel_input(&mut self, n: u8, port: u16) -> u8 {
        let now = self.now;
        let value = match self.ports.parallel.get_mut(usize::from(n)) {
            Some(Some((lpt, base))) => {
                lpt.input(port.wrapping_sub(*base) & 3, now, &mut self.sound.mixer)
            }
            _ => OPEN_BUS,
        };
        self.sync_lines();
        value
    }

    fn parallel_output(&mut self, n: u8, port: u16, value: u8) {
        let now = self.now;
        if let Some(Some((lpt, base))) = self.ports.parallel.get_mut(usize::from(n)) {
            lpt.output(
                port.wrapping_sub(*base) & 3,
                value,
                now,
                &mut self.sound.mixer,
            );
        }
        self.sync_lines();
    }

    /// Brings every sound source to now and renders the mix.
    fn render_audio(&mut self) {
        let now = self.now;
        self.sound.run_fm(now);
        self.sound
            .speaker
            .advance(&self.pit, self.pit_tick, &mut self.sound.mixer);
        for (lpt, _) in self.ports.parallel.iter_mut().flatten() {
            lpt.advance(now, &mut self.sound.mixer);
        }
        self.advance_sound();
        self.sync_lines();
        self.sound.mixer.render(Hertz::CRYSTAL.ticks_at(now));
        self.rendered = now;
    }

    /// A read of a disk adapter's port.
    fn disk_input(&mut self, port: u16) -> Option<u8> {
        let now = self.now;
        let mut bus = BusMemory {
            memory: &mut self.memory,
            cards: &mut self.cards,
            now,
            probe: self.probe.as_deref_mut(),
        };
        let value = match self.decode[usize::from(port) % PORTS] {
            Port::Floppy => self.fdc.as_mut()?.input(port, now, &mut self.dma, &mut bus),
            _ => self.hdc.as_mut()?.input(port, now, &mut self.dma, &mut bus),
        };
        self.sync_lines();
        value
    }

    /// A write to a disk adapter's port.
    fn disk_output(&mut self, port: u16, value: u8) {
        let now = self.now;
        let mut bus = BusMemory {
            memory: &mut self.memory,
            cards: &mut self.cards,
            now,
            probe: self.probe.as_deref_mut(),
        };
        match (
            self.decode[usize::from(port) % PORTS],
            self.fdc.as_mut(),
            self.hdc.as_mut(),
        ) {
            (Port::Floppy, Some(fdc), _) => fdc.output(port, value, now, &mut self.dma, &mut bus),
            (Port::FixedDisk, _, Some(hdc)) => {
                hdc.output(port, value, now, &mut self.dma, &mut bus);
            }
            _ => {}
        }
        self.sync_lines();
    }

    fn output(&mut self, port: u16, value: u8) {
        let low = crate::bits::lo(port);
        match self.decode[usize::from(port) % PORTS] {
            Port::Dma => {
                self.dma.write_port(port, value);
                // A transfer the channel had stalled may go on now.
                self.advance_storage();
                self.advance_sound();
                self.sync_lines();
            }
            Port::Pic => self.pic.write(port & 1 != 0, value),
            Port::Pit => {
                // The speaker hears timer 2 as it was programmed until now.
                self.sound
                    .speaker
                    .advance(&self.pit, self.pit_tick, &mut self.sound.mixer);
                self.pit.write(low, value, self.pit_tick);
            }
            Port::Ppi => {
                if let Some(b) = self.ppi.write(low, value) {
                    self.port_b_changed(b);
                }
            }
            Port::DmaPage => self.dma.write_port(port, value),
            Port::Video(n) => {
                let n = usize::from(n);
                let Some(card) = self.cards.get_mut(n) else {
                    return;
                };
                let moved = card.output(port, value, self.now);
                // A window or port grown over another card's leaves it to the
                // card that had it, as the first to decode an address keeps it.
                if moved.window {
                    self.map_card(n);
                }
                if moved.ports {
                    self.claim_ports(n);
                }
                self.sync_lines();
            }
            Port::Floppy | Port::FixedDisk => self.disk_output(port, value),
            Port::Fm => self.fm_output(port, value),
            Port::Blaster => self.blaster_output(port, value),
            Port::Serial(n) => self.serial_output(n, port, value),
            Port::Parallel(n) => self.parallel_output(n, port, value),
            Port::Game => {
                if let Some(game) = self.ports.game.as_mut() {
                    game.output(self.now);
                }
            }
            Port::Clock => {
                let now = self.now;
                if let Some(card) = self.ports.clock.as_mut() {
                    card.output(port.wrapping_sub(clock::PORT) % clock::PORTS, value, now);
                }
            }
            Port::Ems => {
                if let Some(ems) = self.ems.as_mut() {
                    let n = ems
                        .board
                        .output(port.wrapping_sub(ems.port) % ems::PORTS, value);
                    ems.show(n, &mut self.memory);
                }
            }
            Port::NmiMask => self.nmi_enabled = value & 0x80 != 0,
            Port::None => {}
        }
    }

    /// Gives card `n` the ports it decodes now, where no other device
    /// holds them; returns whether every one was free.
    fn claim_ports(&mut self, n: usize) -> bool {
        let Ok(number) = u8::try_from(n) else {
            return false;
        };
        for slot in &mut self.decode {
            if *slot == Port::Video(number) {
                *slot = Port::None;
            }
        }
        let Some(card) = self.cards.get(n) else {
            return false;
        };
        let mut every = true;
        for &(first, last) in card.ports() {
            for port in first..=last {
                let slot = &mut self.decode[usize::from(port) % PORTS];
                if *slot == Port::None {
                    *slot = Port::Video(number);
                } else {
                    every = false;
                }
            }
        }
        every
    }

    /// Gives card `n` the memory its window now covers, where no other card
    /// holds it.
    fn map_card(&mut self, n: usize) -> bool {
        self.memory.unmap_device(n);
        self.cards.get(n).is_some_and(|card| {
            let (base, len) = card.window();
            self.memory.map_device(base, len, n)
        })
    }

    /// Port B drives timer 2's gate and the keyboard's clock and clear
    /// lines.
    fn port_b_changed(&mut self, b: u8) {
        let mixer = &mut self.sound.mixer;
        self.sound.speaker.advance(&self.pit, self.pit_tick, mixer);
        self.pit
            .set_gate(2, b & port_b::TIMER2_GATE != 0, self.pit_tick);
        let data = b & port_b::SPEAKER_DATA != 0;
        self.sound
            .speaker
            .set_data(data, &self.pit, self.pit_tick, &mut self.sound.mixer);
        self.keyboard.set_lines(
            b & port_b::KEYBOARD_CLOCK != 0,
            b & port_b::KEYBOARD_CLEAR != 0,
            self.now,
        );
        self.pic.set_line(1, self.keyboard.irq());
    }
}

/// Runs the refresh cycles timer 1 requested through `tick`, refresh
/// accounted through tick `accounted`: how many took the bus.
fn refresh_until(pit: &Pit, dma: &mut Dma, accounted: &mut u64, tick: u64) -> u64 {
    if tick <= *accounted {
        return 0;
    }
    let requests = pit.rises_between(REFRESH_TIMER, *accounted, tick);
    *accounted = tick;
    dma.idle_transfers(REFRESH_DMA, requests)
}

/// The CPU cycle `clock` counts by the next refresh after tick
/// `accounted`: never where none will take the bus.
fn refresh_deadline(clock: &Clock, pit: &Pit, dma: &Dma, accounted: u64) -> u64 {
    if !dma.enabled(REFRESH_DMA) {
        return u64::MAX;
    }
    pit.next_rise(REFRESH_TIMER, accounted)
        .map_or(u64::MAX, |tick| {
            clock.first_tick_at(Hertz::PIT.time_of(tick))
        })
}

impl DmaBus for Planar {
    fn memory_read(&mut self, addr: u32) -> u8 {
        self.bus_memory().dma_read(addr)
    }
    fn memory_write(&mut self, addr: u32, value: u8) {
        self.bus_memory().dma_write(addr, value);
    }
    fn io_read(&mut self, port: u16) -> u8 {
        let value = self.input(port);
        self.probe_port(port, value, Cycle::In, Master::Dma);
        value
    }
    fn io_write(&mut self, port: u16, value: u8) {
        self.output(port, value);
        self.probe_port(port, value, Cycle::Out, Master::Dma);
    }
}

/// Gives the sound card, the ports and the clock card their ports, refusing
/// a base or interrupt one cannot be set to or another device holds.
fn claim_adapters(ports: &mut [Port; PORTS], config: &BoardConfig) -> Result<(), ConfigError> {
    let mut claim = |first: u16, count: u16, port: Port, error: ConfigError| {
        for n in first..first + count {
            let slot = &mut ports[usize::from(n) % PORTS];
            if *slot != Port::None {
                return Err(error);
            }
            *slot = port;
        }
        Ok(())
    };
    let sound = ConfigError::SoundConflict;
    match config.sound.card {
        None => {}
        Some(SoundCard::AdLib) => claim(ADLIB, 2, Port::Fm, sound)?,
        Some(SoundCard::Blaster { base, irq }) => {
            let video_irq = config.video.iter().flatten().any(|a| a.own_bios());
            let taken = (irq == hdc::IRQ && config.fixed.is_some()) || (irq == 2 && video_irq);
            if !matches!(base, 0x220 | 0x240) || !matches!(irq, 2 | 3 | 5 | 7) || taken {
                return Err(sound);
            }
            claim(base, 8, Port::Blaster, sound)?;
            claim(base + 8, 2, Port::Fm, sound)?;
            claim(base + 0x0A, 6, Port::Blaster, sound)?;
            claim(ADLIB, 2, Port::Fm, sound)?;
        }
    }
    let conflict = ConfigError::PortConflict;
    for (n, (port, base)) in config.ports.serial.iter().zip(serial::BASES).enumerate() {
        if port.is_some() {
            claim(
                base,
                8,
                Port::Serial(u8::try_from(n).map_err(|_| conflict)?),
                conflict,
            )?;
        }
    }
    for (n, lpt) in config.ports.parallel.iter().enumerate() {
        if let Some(lpt) = lpt {
            if !parallel::BASES.contains(&lpt.base) {
                return Err(conflict);
            }
            let n = u8::try_from(n).map_err(|_| conflict)?;
            claim(lpt.base, parallel::PORTS, Port::Parallel(n), conflict)?;
        }
    }
    if config.ports.game {
        claim(game::PORT, 1, Port::Game, conflict)?;
    }
    if config.ports.clock {
        claim(clock::PORT, clock::PORTS, Port::Clock, conflict)?;
    }
    if let Some(board) = config.ems {
        if !ems::BASES.contains(&board.port) {
            return Err(conflict);
        }
        claim(board.port, ems::PORTS, Port::Ems, conflict)?;
    }
    Ok(())
}

/// Adds an expanded memory board's pages, and the page its manager keeps,
/// to `memory`, and holds its frame.
fn fit_expanded(
    memory: &mut Memory,
    config: Option<EmsConfig>,
) -> Result<Option<Expanded>, ConfigError> {
    let Some(config) = config else {
        return Ok(None);
    };
    if !(1..=ems::MAX_PAGES).contains(&config.pages) || !ems::FRAMES.contains(&config.frame) {
        return Err(ConfigError::Expanded);
    }
    let len = (u32::from(config.pages) + 1) * ems::PAGE;
    let store = memory
        .add_banked(len)
        .map_err(|_| ConfigError::OutOfMemory)?;
    let frame_len = u32::try_from(ems::WINDOWS).map_err(|_| ConfigError::Expanded)? * ems::PAGE;
    memory
        .reserve_frame(config.frame, frame_len)
        .map_err(|_| ConfigError::Expanded)?;
    Ok(Some(Expanded {
        board: Ems::new(config.pages),
        port: config.port,
        frame: config.frame,
        store,
    }))
}

/// The earlier of two instants, either of which may be absent.
fn earliest(a: Option<Nanos>, b: Option<Nanos>) -> Option<Nanos> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The system board.
#[derive(Debug)]
pub struct Board {
    clock: Clock,
    /// The 80186's timer clock: a quarter of the CPU's nominal clock, which
    /// turbo does not change.
    pcb_clock: Option<Hertz>,
    pcb: Option<Pcb>,
    planar: Planar,
    /// Wait states per kilobyte of memory.
    waits: [u8; KIBS],
    /// The wait table changed since the page map last followed it.
    waits_changed: bool,
    /// The page map translated code reaches memory through.
    pages: PageMap,
    /// Cycles the bus took from the CPU since it last asked.
    stolen: u64,
    /// Of those, the cycles the 80186's DMA held the bus for: a halted CPU
    /// waits them out, for its transfers take the bus however idle it is.
    held: u64,
    /// The CPU cycle by which the next refresh takes the bus: checked on
    /// every instruction, so refresh slows the CPU as it runs rather than
    /// when a device is next touched.
    next_refresh: u64,
    /// A device was reprogrammed since the schedule was last read.
    rescheduled: bool,
    intr: bool,
    /// Where the host's pointer stands while the emulator's own mouse driver
    /// follows it.
    host_pointer: Option<HostPointer>,
    /// The folders shared as network drives, a slot each.
    shares: [Option<Share>; SHARES],
    /// What one of the processor's bus cycles carries.
    bus: BusWidth,
    /// The 8087, if fitted.
    fpu: Option<Fpu>,
    nmi: Nmi,
}

/// What one bus cycle carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BusWidth {
    /// The 8088's, V20's and 80188's byte.
    Byte,
    /// The 8086's, V30's and 80186's word.
    Word,
}

/// The NMI pin: the line's level, the 8087's INT behind the mask, and a
/// rising edge the CPU has yet to take.
#[derive(Clone, Copy, Debug, Default)]
struct Nmi {
    line: bool,
    latched: bool,
}

/// The configuration switches `config` sets: the drives, the coprocessor,
/// the display the POST starts on, and the planar's memory.
fn switches(config: &BoardConfig) -> Switches {
    let adapters = || config.video.iter().flatten();
    let display = if adapters().next().is_none() || adapters().any(|a| a.own_bios()) {
        BootDisplay::OwnBios
    } else if adapters().any(|a| a.colour()) {
        BootDisplay::Cga80
    } else {
        BootDisplay::Monochrome
    };
    let floppies = config
        .floppy
        .map_or(0, |adapter| adapter.drives.iter().flatten().count());
    let floppies = u8::try_from(floppies).unwrap_or(4);
    Switches::new(
        config.wiring,
        floppies,
        config.coprocessor,
        display,
        config.memory_kib,
    )
}

impl Board {
    /// A board for `config`, every chip as power-up leaves it.
    pub fn new(config: &BoardConfig) -> Result<Self, ConfigError> {
        let kib = config.memory_kib;
        if !(64..=640).contains(&kib) || !kib.is_multiple_of(64) {
            return Err(ConfigError::Memory);
        }
        let mut memory =
            Memory::new(u32::from(kib) * 1024).map_err(|_| ConfigError::OutOfMemory)?;
        let ems = fit_expanded(&mut memory, config.ems)?;
        let mut ports = [Port::None; PORTS];
        for (port, slot) in ports.iter_mut().enumerate().take(0x100) {
            // The planar decodes A9–A5 into blocks of 32 ports.
            *slot = match port >> 5 {
                0 => Port::Dma,
                1 => Port::Pic,
                2 => Port::Pit,
                3 => Port::Ppi,
                4 => Port::DmaPage,
                5 => Port::NmiMask,
                _ => Port::None,
            };
        }
        if config.floppy.is_some() {
            ports[0x3F0..=0x3F7].fill(Port::Floppy);
        }
        if config.fixed.is_some() {
            ports[0x320..=0x323].fill(Port::FixedDisk);
        }
        claim_adapters(&mut ports, config)?;
        let mut cards = Vec::new();
        cards
            .try_reserve_exact(2)
            .map_err(|_| ConfigError::OutOfMemory)?;
        for adapter in config.video.iter().flatten() {
            cards.push(Card::new(*adapter).map_err(|_| ConfigError::OutOfMemory)?);
        }
        let switches = switches(config);
        let transfers = config.clock.divided::<DMA_CYCLES>();
        let has_pcb = config.cpu.family() == Family::Intel80186;
        let mut board = Self {
            clock: Clock::new(config.clock.rate()),
            pcb_clock: has_pcb.then(|| config.clock.divided::<4>()),
            pcb: has_pcb.then(|| Pcb::new(config.cpu.bus8())),
            planar: Planar {
                memory,
                decode: ports,
                pic: Pic::new(),
                pit: Pit::new(),
                dma: Dma::new(),
                ppi: Ppi::new(config.wiring, switches),
                keyboard: Keyboard::new(),
                cards,
                fdc: config
                    .floppy
                    .map(|adapter| Fdc::new(adapter.interface, adapter.drives)),
                hdc: config
                    .fixed
                    .map(|adapter| Hdc::new(adapter.recording, adapter.jumpers, transfers)),
                sound: Sound::new(config.sound)?,
                ports: Ports::new(&config.ports),
                ems,
                rendered: 0,
                now: 0,
                pit_tick: 0,
                refresh_tick: 0,
                nmi_enabled: false,
                probe: None,
            },
            waits: [MEMORY_WAIT_STATES; KIBS],
            waits_changed: true,
            pages: PageMap::default(),
            stolen: 0,
            held: 0,
            next_refresh: u64::MAX,
            rescheduled: false,
            intr: false,
            host_pointer: None,
            shares: [const { None }; SHARES],
            bus: if config.cpu.bus8() {
                BusWidth::Byte
            } else {
                BusWidth::Word
            },
            fpu: config.coprocessor.then(Fpu::new),
            nmi: Nmi::default(),
        };
        for n in 0..board.planar.cards.len() {
            if !board.planar.claim_ports(n) || !board.planar.map_card(n) {
                return Err(ConfigError::VideoConflict);
            }
        }
        let lines = board.planar.ppi.read_b();
        board.planar.port_b_changed(lines);
        board.chip_selects_changed();
        board.sync_interrupts();
        Ok(board)
    }

    /// Maps `image` as ROM at `base`; `host` marks it the emulator's own.
    pub fn map_rom(&mut self, base: u32, image: &[u8], host: bool) -> Result<(), MapError> {
        self.planar.memory.map_rom(base, image, host)
    }

    /// The emulated time at which CPU cycle `cycles` happens.
    #[must_use]
    pub fn time_of(&self, cycles: u64) -> Nanos {
        self.clock.time_of(cycles)
    }

    /// The first CPU cycle at or after emulated time `t`.
    #[must_use]
    pub fn cycles_at(&self, t: Nanos) -> u64 {
        self.clock.first_tick_at(t)
    }

    /// Brings every device up to `t`, the cards drawn to it too: how a
    /// snapshot is taken, so every part stands at the board's time.
    pub fn settle(&mut self, t: Nanos) {
        self.advance_to(t);
        let now = self.planar.now;
        for card in &mut self.planar.cards {
            card.catch_up(now);
        }
    }

    /// The emulated time the devices have been brought up to.
    #[must_use]
    pub const fn devices_now(&self) -> Nanos {
        self.planar.now
    }

    /// The rate the CPU runs at now.
    #[must_use]
    pub const fn cpu_rate(&self) -> Hertz {
        self.clock.rate()
    }

    /// Runs the CPU at `rate` from cycle `cycles` on. The devices keep
    /// their own clocks, but for the DMA controller, which runs on the
    /// CPU's.
    pub fn set_cpu_rate(&mut self, rate: CpuClock, cycles: u64) {
        self.clock.set_rate(rate.rate(), cycles);
        if let Some(hdc) = self.planar.hdc.as_mut() {
            hdc.set_transfer_rate(rate.divided::<DMA_CYCLES>());
        }
        self.schedule_refresh();
    }

    /// Catches every device up to emulated time `t`, charging the CPU for
    /// the bus cycles DMA took meanwhile.
    pub fn advance_to(&mut self, t: Nanos) {
        self.planar.advance_to(t);
        let refreshes = self
            .planar
            .refresh_until(Planar::pit_tick_at(self.planar.now));
        self.stolen += refreshes * DMA_CYCLES;
        self.charge_transfers();
        if let (Some(pcb), Some(rate)) = (self.pcb.as_mut(), self.pcb_clock) {
            pcb.advance(rate.ticks_at(self.planar.now));
        }
        self.run_pcb_dma();
        self.complete_coprocessor();
        self.schedule_refresh();
        self.sync_interrupts();
    }

    /// Commits the 8087's instruction in flight once the devices' time has
    /// reached its end.
    fn complete_coprocessor(&mut self) {
        let due = self
            .fpu
            .as_ref()
            .and_then(Fpu::ends)
            .filter(|&ends| self.clock.time_of(ends) <= self.planar.now);
        let Some(ends) = due else {
            return;
        };
        if let Some(mut fpu) = self.fpu.take() {
            fpu.complete(&mut Coprocessor::new(self, ends));
            self.fpu = Some(fpu);
        }
    }

    /// Runs the transfers the 80186's DMA wants now, the CPU charged for the
    /// bus they took.
    fn run_pcb_dma(&mut self) {
        if let Some(pcb) = self.pcb.as_mut() {
            pcb.run_dma(&mut self.planar);
            let taken = pcb.take_stolen();
            self.stolen += taken;
            self.held += taken;
        }
    }

    /// Catches every device up to `t` across a span the CPU spent halted:
    /// the refreshes and the disks' transfers cost it nothing, but it waits
    /// out the cycles the 80186's DMA held the bus for, which it answers.
    pub fn idle_to(&mut self, t: Nanos) -> u64 {
        self.advance_to(t);
        self.stolen = 0;
        core::mem::take(&mut self.held)
    }

    /// Charges the CPU for the bus cycles the disks' DMA transfers took.
    fn charge_transfers(&mut self) {
        self.stolen += self.planar.dma.take_transfers() * DMA_CYCLES;
    }

    fn schedule_refresh(&mut self) {
        let planar = &self.planar;
        self.next_refresh =
            refresh_deadline(&self.clock, &planar.pit, &planar.dma, planar.refresh_tick);
    }

    /// When a device next needs to act, if one will: after the time last
    /// caught up to. Reading the schedule clears the report that a device
    /// was reprogrammed since it was last read.
    pub fn next_event(&mut self) -> Option<Nanos> {
        self.rescheduled = false;
        let pcb = self.pcb.as_ref().and_then(|pcb| {
            if pcb.dma_pending() {
                return Some(self.planar.now + 1);
            }
            let rate = self.pcb_clock?;
            pcb.next_event().map(|clock| rate.time_of(clock))
        });
        let coprocessor = self
            .fpu
            .as_ref()
            .and_then(Fpu::ends)
            .map(|ends| self.clock.time_of(ends));
        earliest(earliest(self.planar.next_event(), pcb), coprocessor)
    }

    /// A key goes down or comes up now.
    pub fn key(&mut self, key: Scan, down: bool) {
        if down {
            self.planar.keyboard.press(key, self.planar.now);
        } else {
            self.planar.keyboard.release(key);
        }
    }

    /// The keys held down, the lowest code first.
    pub fn held_keys(&self) -> impl Iterator<Item = Scan> + '_ {
        self.planar.keyboard.held_keys()
    }

    /// Whether any key is held down.
    #[must_use]
    pub fn keys_held(&self) -> bool {
        self.planar.keyboard.any_held()
    }

    /// Whether key `key` going down (`down`) or up now fits in the
    /// keyboard's buffer rather than overrunning it.
    #[must_use]
    pub fn key_fits(&self, key: Scan, down: bool) -> bool {
        self.planar.keyboard.fits(key, down)
    }

    /// Whether the guest has read every byte the keyboard had to send.
    #[must_use]
    pub const fn keyboard_idle(&self) -> bool {
        self.planar.keyboard.is_idle()
    }

    /// Puts `floppy` in drive `n` now, returning the disk it held. Without
    /// the drive the disk comes back.
    pub fn insert_floppy(&mut self, n: usize, floppy: Floppy) -> Result<Option<Floppy>, Floppy> {
        let Some(fdc) = self.planar.fdc.as_mut() else {
            return Err(floppy);
        };
        let held = fdc.insert(n, floppy, self.planar.now);
        self.disk_changed();
        held
    }

    /// Takes the disk out of drive `n` now.
    pub fn eject_floppy(&mut self, n: usize) -> Option<Floppy> {
        let held = self.planar.fdc.as_mut()?.eject(n, self.planar.now);
        self.disk_changed();
        held
    }

    /// Attaches fixed disk `n`: a drive of `kind` holding `device`,
    /// formatted as `format`.
    pub fn attach_fixed(
        &mut self,
        n: usize,
        kind: DiskType,
        device: Box<dyn BlockDevice>,
        format: LowLevel,
    ) -> Result<(), AttachError> {
        let hdc = self.planar.hdc.as_mut().ok_or(AttachError::NoSuchDrive)?;
        hdc.attach(n, kind, device, format)
    }

    /// Fixed disk `n`'s low-level format as it stands, for its owner to
    /// keep beside the image.
    #[must_use]
    pub fn fixed_format(&self, n: usize) -> Option<&LowLevel> {
        self.planar.hdc.as_ref()?.low_level(n)
    }

    /// What the drives' lamps show now.
    #[must_use]
    pub fn lights(&self) -> Lights {
        Lights {
            floppy: self.planar.fdc.as_ref().map_or(0, Fdc::motors),
            fixed: self
                .planar
                .hdc
                .as_ref()
                .and_then(Hdc::active)
                .map_or(0, |n| 1 << n),
        }
    }

    fn disk_changed(&mut self) {
        self.planar.sync_lines();
        self.rescheduled = true;
        self.sync_interrupts();
    }

    /// The serial mouse moves by `dx`, `dy` (down positive) with `buttons`
    /// held, now; returns whether a port has one. The mouse is the guest's
    /// alone from now on: the host's pointer is no longer followed.
    pub fn mouse(&mut self, dx: i32, dy: i32, buttons: u8) -> bool {
        self.host_pointer = None;
        self.serial_mouse(dx, dy, buttons, false)
    }

    /// The host's pointer stands at `at` over the picture with `buttons`
    /// held: the emulator's own driver moves its pointer there, and the
    /// serial mouse reports the change to any other driver as the motion of
    /// a mouse whose default ratios cover a 640 by 200 screen. Returns
    /// whether a port has a mouse.
    pub fn point(&mut self, at: HostPointer, buttons: u8) -> bool {
        let counts = |p: HostPointer| {
            let along = |fraction: u16, extent: i32| (i32::from(fraction) * extent) >> 16;
            (along(p.x, 640), 2 * along(p.y, 200))
        };
        let (dx, dy) = self.host_pointer.map_or((0, 0), |from| {
            let ((x0, y0), (x1, y1)) = (counts(from), counts(at));
            (x1 - x0, y1 - y0)
        });
        self.host_pointer = Some(at);
        self.serial_mouse(dx, dy, buttons, true)
    }

    /// Where the host's pointer stands, while the emulator's own mouse driver
    /// follows it.
    #[must_use]
    pub const fn host_pointer(&self) -> Option<HostPointer> {
        self.host_pointer
    }

    /// The emulator's own driver no longer follows the host's pointer.
    pub fn release_pointer(&mut self) {
        self.host_pointer = None;
    }

    fn serial_mouse(&mut self, dx: i32, dy: i32, buttons: u8, report: bool) -> bool {
        let now = self.planar.now;
        let moved = self
            .planar
            .ports
            .serial
            .iter_mut()
            .flatten()
            .any(|uart| uart.mouse(dx, dy, buttons, report, now));
        if moved {
            self.planar.sync_lines();
            self.rescheduled = true;
            self.sync_interrupts();
        }
        moved
    }

    /// The joysticks on the game port stand as `sticks` from now on;
    /// returns whether a game port is fitted.
    pub fn set_sticks(&mut self, sticks: Sticks) -> bool {
        self.planar
            .ports
            .game
            .as_mut()
            .map(|game| game.set(sticks))
            .is_some()
    }

    /// Moves what the printer on parallel adapter `n` printed into `out`;
    /// returns the bytes moved.
    pub fn take_printed(&mut self, n: usize, out: &mut [u8]) -> usize {
        match self.planar.ports.parallel.get_mut(n) {
            Some(Some((lpt, _))) => lpt.take_printed(out),
            _ => 0,
        }
    }

    /// Share `share` as slot `slot`, below [`SHARES`], in place of what it
    /// held; answers what it held.
    pub fn share(&mut self, slot: u8, share: Share) -> Option<Share> {
        self.shares.get_mut(usize::from(slot))?.replace(share)
    }

    /// Stop sharing slot `slot`; answers what it held.
    pub fn unshare(&mut self, slot: u8) -> Option<Share> {
        self.shares.get_mut(usize::from(slot))?.take()
    }

    /// The share in slot `slot`.
    pub fn share_mut(&mut self, slot: u8) -> Option<&mut Share> {
        self.shares.get_mut(usize::from(slot))?.as_mut()
    }

    /// The slot of the share that is drive `drive`.
    #[must_use]
    pub fn shared_drive(&self, drive: u8) -> Option<u8> {
        let slot = self.shares.iter().position(|share| {
            share
                .as_ref()
                .is_some_and(|share| share.drive == Some(drive))
        })?;
        u8::try_from(slot).ok()
    }

    /// Every share, by slot.
    pub fn shares_mut(&mut self) -> impl Iterator<Item = (u8, &mut Share)> {
        self.shares
            .iter_mut()
            .zip(0u8..)
            .filter_map(|(share, slot)| share.as_mut().map(|share| (slot, share)))
    }

    /// Sets the clock card to `now`; refuses a time it cannot hold, or a
    /// board without the card.
    pub fn set_clock(&mut self, now: &CivilTime) -> bool {
        let t = self.planar.now;
        self.planar
            .ports
            .clock
            .as_mut()
            .is_some_and(|card| card.set(now, t))
    }

    /// The time and date the clock card holds, if it holds one.
    pub fn clock(&mut self) -> Option<CivilTime> {
        let t = self.planar.now;
        self.planar.ports.clock.as_mut()?.time(t)
    }

    /// The time the guest set the clock card to, once it has let the card
    /// run on with it: taken once a setting.
    pub fn clock_set(&mut self) -> Option<CivilTime> {
        let t = self.planar.now;
        self.planar.ports.clock.as_mut()?.take_guest_setting(t)
    }

    /// The expanded memory board's store, which its manager reaches
    /// directly: its pages in order, then the page it keeps its tables in.
    pub fn expanded_store(&mut self) -> Option<&mut [u8]> {
        let ems = self.planar.ems.as_ref()?;
        let len = (u32::from(ems.board.pages()) + 1) * ems::PAGE;
        self.planar.memory.store_mut(ems.store, len)
    }

    fn catch_up(&mut self, at: u64) {
        self.advance_to(self.clock.time_of(at));
    }

    /// The NMI line's level now.
    fn nmi_line(&self) -> bool {
        self.planar.nmi_enabled && self.fpu.as_ref().is_some_and(Fpu::interrupting)
    }

    fn sync_interrupts(&mut self) {
        let line = self.nmi_line();
        if line && !self.nmi.line {
            self.nmi.latched = true;
        }
        self.nmi.line = line;
        let pic = self.planar.pic.intr();
        self.intr = match self.pcb.as_mut() {
            Some(pcb) => {
                pcb.set_int_pin(0, pic);
                if pcb.slave_mode() {
                    pic
                } else {
                    pcb.intr()
                }
            }
            None => pic,
        };
    }

    /// The control block's offset for an I/O port it decodes.
    fn pcb_port(&self, port: u16) -> Option<u8> {
        match self.pcb.as_ref()?.space() {
            Space::Io(base) => port.checked_sub(base).and_then(|o| u8::try_from(o).ok()),
            Space::Memory(_) => None,
        }
    }

    /// The control block's offset for a memory address it overlays.
    fn pcb_address(&self, addr: u32) -> Option<u8> {
        if !self.planar.memory.overlaid(addr) {
            return None;
        }
        match self.pcb.as_ref()?.space() {
            Space::Memory(base) => addr.checked_sub(base).and_then(|o| u8::try_from(o).ok()),
            Space::Io(_) => None,
        }
    }

    fn pcb_effect(&mut self, effect: Effect) {
        match effect {
            Effect::None => {}
            Effect::Relocated => {
                let overlay = match self.pcb.as_ref().map(Pcb::space) {
                    Some(Space::Memory(base)) => Some(base),
                    _ => None,
                };
                self.planar.memory.set_overlay(overlay);
            }
            Effect::ChipSelects => self.chip_selects_changed(),
        }
        self.run_pcb_dma();
        self.rescheduled = true;
        self.sync_interrupts();
    }

    /// Recomputes the memory wait table from the chip-select unit.
    fn chip_selects_changed(&mut self) {
        self.waits_changed = true;
        let Some(pcb) = self.pcb.as_ref() else { return };
        let granule = pcb.memory_granule();
        for (kib, slot) in (0u32..).zip(self.waits.iter_mut()) {
            let base = kib << 10;
            let at = |addr: u32| {
                pcb.memory_waits(addr)
                    .map_or(MEMORY_WAIT_STATES, |w| w.with_board(MEMORY_WAIT_STATES))
            };
            let first = at(base);
            let uniform = (1..1024 / granule).all(|i| at(base + i * granule) == first);
            *slot = if uniform { first } else { MIXED };
        }
    }

    fn memory_waits(&self, addr: u32) -> u8 {
        match self.waits[(addr >> 10) as usize % KIBS] {
            MIXED => self
                .pcb
                .as_ref()
                .and_then(|pcb| pcb.memory_waits(addr))
                .map_or(MEMORY_WAIT_STATES, |w| w.with_board(MEMORY_WAIT_STATES)),
            waits => waits,
        }
    }

    fn io_waits(&self, port: u16) -> u8 {
        self.pcb
            .as_ref()
            .and_then(|pcb| pcb.io_waits(port))
            .map_or(IO_WAIT_STATES, |w| w.with_board(IO_WAIT_STATES))
    }
}

impl Board {
    /// The CPU cycles an access at cycle `at` waits for a card's memory.
    fn card_wait(&self, n: usize, at: u64) -> u64 {
        let t = self.clock.time_of(at);
        let wait = self
            .planar
            .cards
            .get(n)
            .map_or(0, |card| card.access_wait(t));
        self.clock.first_tick_at(t + wait).saturating_sub(at)
    }

    /// Brings every sound source to now and renders the mix into the audio
    /// stream, but for the last instants a later change could still reach.
    pub fn render_audio(&mut self) {
        self.planar.render_audio();
    }

    /// Moves rendered audio into `out` as interleaved stereo frames;
    /// returns the frames moved.
    pub fn take_audio(&mut self, out: &mut [f32]) -> usize {
        self.planar.sound.mixer.take(out)
    }

    /// Steers the mix toward holding the front-end's audio ring at `target`
    /// frames when it holds `fill`; called once each time it is fed.
    pub fn steer_audio(&mut self, fill: u32, target: u32) {
        self.planar.sound.mixer.steer(fill, target);
    }

    /// Sets `source`'s level in the mix.
    pub fn set_audio_gain(&mut self, source: Source, gain: f32) {
        self.planar.sound.mixer.set_gain(source, gain);
    }

    /// Audio lost to memory or to nothing draining the stream.
    #[must_use]
    pub fn audio_dropped(&self) -> u64 {
        self.planar.sound.mixer.dropped()
    }

    /// Whether display adapter `n` is in a text mode.
    #[must_use]
    pub fn shows_text(&self, n: usize) -> bool {
        self.planar.cards.get(n).is_some_and(Card::shows_text)
    }

    /// The text display adapter `n` shows, as its registers and memory
    /// stand.
    ///
    /// # Errors
    ///
    /// As [`crate::video::Card::shown_text`]; [`Unread::NoText`] for an
    /// adapter the machine lacks.
    pub fn shown_text(&self, n: usize) -> Result<ShownText, Unread> {
        self.planar
            .cards
            .get(n)
            .map_or(Err(Unread::NoText), Card::shown_text)
    }

    /// The 8087, if fitted.
    #[must_use]
    pub const fn coprocessor(&self) -> Option<&Fpu> {
        self.fpu.as_ref()
    }

    /// The last finished frame of display adapter `n`, caught up to now, if
    /// one finished since the last call.
    pub fn frame(&mut self, n: usize) -> Option<Frame<'_>> {
        let now = self.planar.now;
        let card = self.planar.cards.get_mut(n)?;
        card.catch_up(now);
        card.take_frame()
    }

    /// A read of `addr` by `master`, none for a code fetch, which no
    /// watch sees.
    fn read_by(&mut self, addr: u32, at: u64, master: Option<Master>) -> u8 {
        if self.planar.memory.special(addr) {
            return self.read_special(addr, at, master);
        }
        self.stolen += u64::from(self.memory_waits(addr));
        self.planar.memory.read(addr)
    }

    fn write_by(&mut self, addr: u32, value: u8, at: u64, master: Master) {
        if self.planar.memory.special(addr) {
            self.write_special(addr, value, at, master);
            return;
        }
        self.stolen += u64::from(self.memory_waits(addr));
        self.planar.memory.write(addr, value);
    }

    fn read_special(&mut self, addr: u32, at: u64, master: Option<Master>) -> u8 {
        if let Some(offset) = self.pcb_address(addr) {
            self.catch_up(at);
            let value = self
                .pcb
                .as_mut()
                .map_or(OPEN_BUS, |pcb| pcb.read_byte(offset));
            self.sync_interrupts();
            return value;
        }
        self.stolen += u64::from(self.memory_waits(addr));
        let value = match self.planar.memory.device(addr) {
            Some(n) => {
                self.stolen += self.card_wait(n, at);
                self.planar
                    .cards
                    .get_mut(n)
                    .map_or(OPEN_BUS, |card| card.read(addr))
            }
            None => self.planar.memory.read(addr),
        };
        if let Some(master) = master {
            self.planar.probe_memory(addr, value, Cycle::Read, master);
        }
        value
    }

    fn write_special(&mut self, addr: u32, value: u8, at: u64, master: Master) {
        if let Some(offset) = self.pcb_address(addr) {
            self.catch_up(at);
            let effect = self
                .pcb
                .as_mut()
                .map_or(Effect::None, |pcb| pcb.write_byte(offset, value));
            self.pcb_effect(effect);
            return;
        }
        self.stolen += u64::from(self.memory_waits(addr));
        match self.planar.memory.device(addr) {
            Some(n) => {
                self.stolen += self.card_wait(n, at);
                let t = self.clock.time_of(at);
                if let Some(card) = self.planar.cards.get_mut(n) {
                    card.write(addr, value, t);
                }
            }
            None => self.planar.memory.write(addr, value),
        }
        self.planar.probe_memory(addr, value, Cycle::Write, master);
    }
}

impl Bus for Board {
    fn read(&mut self, addr: u32, at: u64) -> u8 {
        self.read_by(addr, at, Some(Master::Cpu))
    }

    fn write(&mut self, addr: u32, value: u8, at: u64) {
        self.write_by(addr, value, at, Master::Cpu);
    }

    fn fetch(&mut self, addr: u32, at: u64) -> u8 {
        self.read_by(addr, at, None)
    }

    fn peek(&self, addr: u32) -> u8 {
        match (self.pcb_address(addr), self.pcb.as_ref()) {
            (Some(offset), Some(pcb)) => pcb.peek_byte(offset),
            _ => match self.planar.memory.device(addr) {
                Some(n) => self
                    .planar
                    .cards
                    .get(n)
                    .map_or(OPEN_BUS, |card| card.peek(addr)),
                None => self.planar.memory.read(addr),
            },
        }
    }

    fn input(&mut self, port: u16, at: u64) -> u8 {
        self.catch_up(at);
        let value = if let Some(offset) = self.pcb_port(port) {
            let value = self
                .pcb
                .as_mut()
                .map_or(OPEN_BUS, |pcb| pcb.read_byte(offset));
            self.sync_interrupts();
            value
        } else {
            self.stolen += u64::from(self.io_waits(port));
            let value = self.planar.input(port);
            self.charge_transfers();
            self.sync_interrupts();
            value
        };
        self.planar.probe_port(port, value, Cycle::In, Master::Cpu);
        value
    }

    fn output(&mut self, port: u16, value: u8, at: u64) {
        self.catch_up(at);
        if let Some(offset) = self.pcb_port(port) {
            let effect = self
                .pcb
                .as_mut()
                .map_or(Effect::None, |pcb| pcb.write_byte(offset, value));
            self.pcb_effect(effect);
        } else {
            self.stolen += u64::from(self.io_waits(port));
            self.planar.output(port, value);
            self.charge_transfers();
            self.schedule_refresh();
            self.rescheduled = true;
            self.sync_interrupts();
        }
        self.planar.probe_port(port, value, Cycle::Out, Master::Cpu);
    }

    fn input_word(&mut self, port: u16, at: u64) -> u16 {
        if let Some(offset) = self.pcb_port(port).filter(|offset| offset & 1 == 0) {
            self.catch_up(at);
            let value = self.pcb.as_mut().map_or(0xFFFF, |pcb| pcb.read(offset));
            self.sync_interrupts();
            self.planar.probe_port_word(port, value, Cycle::In);
            return value;
        }
        let low = self.input(port, at);
        word(low, self.input(port.wrapping_add(1), at))
    }

    fn output_word(&mut self, port: u16, value: u16, at: u64) {
        if let Some(offset) = self.pcb_port(port).filter(|offset| offset & 1 == 0) {
            self.catch_up(at);
            let effect = self
                .pcb
                .as_mut()
                .map_or(Effect::None, |pcb| pcb.write(offset, value));
            self.pcb_effect(effect);
            self.planar.probe_port_word(port, value, Cycle::Out);
            return;
        }
        self.output(port, crate::bits::lo(value), at);
        self.output(port.wrapping_add(1), crate::bits::hi(value), at);
    }

    fn intr(&self) -> bool {
        self.intr
    }

    fn acknowledge(&mut self) -> u8 {
        let internal = self
            .pcb
            .as_mut()
            .filter(|pcb| !pcb.slave_mode())
            .map(Pcb::acknowledge);
        let vector = match internal {
            None | Some(Some(Ack::Cascade(0))) => self.planar.pic.acknowledge(),
            Some(Some(Ack::Vector(vector))) => vector,
            // Nothing drives the bus: INT1 has no controller behind it.
            Some(_) => OPEN_BUS,
        };
        self.sync_interrupts();
        vector
    }

    fn nmi(&self) -> bool {
        self.nmi.latched
    }

    fn acknowledge_nmi(&mut self) {
        self.nmi.latched = false;
        // The 80186 halts its DMA as it takes NMI, until the IRET.
        if let Some(pcb) = self.pcb.as_mut() {
            pcb.nmi_taken();
        }
    }

    fn interrupt_return(&mut self) {
        if let Some(pcb) = self.pcb.as_mut() {
            pcb.interrupt_returned();
            self.run_pcb_dma();
            self.sync_interrupts();
        }
    }

    fn host_code(&self, addr: u32) -> bool {
        self.planar.memory.host_code(addr)
    }

    fn escape_traps(&self) -> bool {
        self.pcb.as_ref().is_some_and(Pcb::escape_traps)
    }

    fn escape(&mut self, escape: Escape, at: u64) {
        self.catch_up(at);
        let Some(mut fpu) = self.fpu.take() else {
            return;
        };
        fpu.escape(escape, at, &mut Coprocessor::new(self, at));
        self.fpu = Some(fpu);
        // Its end is a new event, and what it did may move NMI.
        self.rescheduled = true;
        self.sync_interrupts();
    }

    /// An empty socket leaves TEST floating high: a WAIT never ends.
    fn test_low_at(&mut self, at: u64) -> u64 {
        self.catch_up(at);
        self.fpu.as_ref().map_or(u64::MAX, |fpu| fpu.ready_at(at))
    }

    fn take_wait_states(&mut self, at: u64) -> u32 {
        self.refresh().take(at)
    }

    fn waiting(&self) -> bool {
        self.stolen != 0
    }

    fn schedule_changed(&self) -> bool {
        self.rescheduled
    }

    fn next_refresh(&self) -> u64 {
        self.next_refresh
    }

    fn steal(&mut self, cycles: u32) {
        self.stolen += u64::from(cycles);
    }

    fn begins(&mut self, cpu: &Cpu) -> bool {
        self.probe_begins(cpu)
    }

    fn interrupts(&mut self, vector: u8, cpu: &Cpu, cause: Cause) {
        if let Some(probe) = self.planar.probe.as_deref_mut() {
            probe.interrupt(vector, cpu, cause);
        }
    }

    fn watches_instructions(&self) -> bool {
        self.planar
            .probe
            .as_deref()
            .is_some_and(Probe::watches_instructions)
    }
}

/// The board as the 8087 reaches it, bus master between the CPU's cycles:
/// one request/grant exchange, then a bus cycle a transfer, each held from
/// the CPU, and the wait states each address carries.
struct Coprocessor<'a> {
    board: &'a mut Board,
    at: u64,
    granted: bool,
    /// The address last moved, whose word partner a 16-bit bus carries in
    /// the same cycle.
    last: Option<u32>,
}

impl<'a> Coprocessor<'a> {
    fn new(board: &'a mut Board, at: u64) -> Self {
        Self {
            board,
            at,
            granted: false,
            last: None,
        }
    }

    fn transfer(&mut self, addr: u32) {
        if !self.granted {
            self.board.stolen += EXCHANGE_CYCLES;
            self.granted = true;
        }
        let paired = self.board.bus == BusWidth::Word
            && addr & 1 == 1
            && self.last == Some(addr.wrapping_sub(1));
        if !paired {
            self.board.stolen += u64::from(BUS_CYCLE);
        }
        self.last = Some(addr);
    }
}

impl fpu::Memory for Coprocessor<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        self.transfer(addr);
        self.board.read_by(addr, self.at, Some(Master::Coprocessor))
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.transfer(addr);
        self.board
            .write_by(addr, value, self.at, Master::Coprocessor);
    }
}

#[cfg(test)]
#[path = "board_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "coprocessor_tests.rs"]
mod coprocessor_tests;
