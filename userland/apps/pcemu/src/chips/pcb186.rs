//! The 80186/80188's integrated peripherals behind the peripheral control
//! block: the interrupt controller, three timers, two DMA channels and the
//! chip-select unit, in the 256 bytes the relocation register places in I/O
//! or memory space.
//!
//! On this board the 8259A drives INT0, which firmware puts in cascade mode
//! so the 8259A supplies its own vectors. No timer pin and no DMA request
//! pin is wired, so a timer counts only the internal clock (a quarter of the
//! CPU's) and a DMA channel runs only unsynchronised or on timer 2's
//! requests. In slave (iRMX) mode the controller's request output, INT3,
//! reaches nothing, so the 8259A interrupts the CPU directly; the slave
//! mode's registers then hold what is written to them.

use crate::bits::{high16, lo, low16, word};

/// Interrupt sources, in the order that breaks ties within a level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Timer,
    Dma0,
    Dma1,
    Int0,
    Int1,
    Int2,
    Int3,
}

const SOURCES: [Source; 7] = [
    Source::Timer,
    Source::Dma0,
    Source::Dma1,
    Source::Int0,
    Source::Int1,
    Source::Int2,
    Source::Int3,
];

impl Source {
    /// The source's bit in IMASK, INSERV and REQST.
    const fn bit(self) -> u16 {
        match self {
            Self::Timer => 0x01,
            Self::Dma0 => 0x04,
            Self::Dma1 => 0x08,
            Self::Int0 => 0x10,
            Self::Int1 => 0x20,
            Self::Int2 => 0x40,
            Self::Int3 => 0x80,
        }
    }

    const fn index(self) -> usize {
        self as usize
    }

    /// The interrupt type the source vectors to; the timers' is timer 0's.
    const fn vector(self) -> u8 {
        match self {
            Self::Timer => 8,
            Self::Dma0 => 10,
            Self::Dma1 => 11,
            Self::Int0 => 12,
            Self::Int1 => 13,
            Self::Int2 => 14,
            Self::Int3 => 15,
        }
    }

    /// The source a specific EOI's type names: 8 for every timer.
    fn of_type(vector: u8) -> Option<Self> {
        SOURCES.into_iter().find(|s| s.vector() == vector)
    }
}

/// What an interrupt acknowledge resolved to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ack {
    /// An internal source's own vector.
    Vector(u8),
    /// INT0 (`0`) or INT1 (`1`) in cascade mode: the external controller
    /// drives the vector.
    Cascade(u8),
}

/// The interrupt control registers' bits.
mod con {
    pub const PRIORITY: u16 = 0x0007;
    pub const MASK: u16 = 0x0008;
    pub const LEVEL: u16 = 0x0010;
    pub const CASCADE: u16 = 0x0020;
    pub const NESTED: u16 = 0x0040;
}

/// INTSTS's DMA halt bit.
const DMA_HALT: u16 = 0x8000;

#[derive(Clone, Debug)]
struct Icu {
    /// TCUCON, DMA0CON, DMA1CON, I0CON–I3CON.
    control: [u16; 7],
    priority_mask: u16,
    in_service: u16,
    request: u16,
    /// INTSTS: the DMA halt and which timers request.
    status: u16,
    pins: u8,
}

impl Icu {
    const fn new() -> Self {
        Self {
            control: [con::PRIORITY | con::MASK; 7],
            priority_mask: 7,
            in_service: 0,
            request: 0,
            status: 0,
            pins: 0,
        }
    }

    fn priority(&self, s: Source) -> u16 {
        self.control[s.index()] & con::PRIORITY
    }

    fn has(&self, s: Source, bit: u16) -> bool {
        self.control[s.index()] & bit != 0
    }

    /// The highest-priority request that may interrupt now: unmasked, within
    /// the priority mask, and above every level in service, except that a
    /// source in special fully nested mode may interrupt its own service.
    fn pending(&self) -> Option<Source> {
        let serving = |s: Source| {
            SOURCES
                .into_iter()
                .filter(|&t| {
                    self.in_service & t.bit() != 0 && !(t == s && self.has(s, con::NESTED))
                })
                .map(|t| self.priority(t))
                .min()
        };
        SOURCES
            .into_iter()
            .filter(|&s| self.request & s.bit() != 0 && !self.has(s, con::MASK))
            .filter(|&s| self.priority(s) <= self.priority_mask)
            .filter(|&s| serving(s).is_none_or(|level| self.priority(s) < level))
            .min_by_key(|&s| (self.priority(s), s.index()))
    }

    /// The requesting timer's status bit, timer 0's first.
    fn timer(&self) -> u16 {
        let requesting = self.status & 7;
        requesting & requesting.wrapping_neg()
    }

    fn vector_of(&self, s: Source) -> u8 {
        match (s, self.timer()) {
            (Source::Timer, 2) => 18,
            (Source::Timer, 4) => 19,
            _ => s.vector(),
        }
    }

    fn take(&mut self, s: Source) -> Ack {
        self.in_service |= s.bit();
        let vector = self.vector_of(s);
        if s == Source::Timer {
            self.status &= !self.timer();
            if self.timer() == 0 {
                self.request &= !s.bit();
            }
        } else if !self.has(s, con::LEVEL) {
            self.request &= !s.bit();
        }
        match s {
            Source::Int0 | Source::Int1 if self.has(s, con::CASCADE) => {
                Ack::Cascade(u8::from(s == Source::Int1))
            }
            _ => Ack::Vector(vector),
        }
    }

    fn eoi(&mut self, value: u16) {
        let source = if value & 0x8000 != 0 {
            SOURCES
                .into_iter()
                .filter(|s| self.in_service & s.bit() != 0)
                .min_by_key(|&s| (self.priority(s), s.index()))
        } else {
            Source::of_type(lo(value) & 0x1F)
        };
        if let Some(s) = source {
            self.in_service &= !s.bit();
        }
    }

    fn set_pin(&mut self, n: u8, high: bool) {
        let source = SOURCES[3 + usize::from(n & 3)];
        let bit = 1u8 << (n & 3);
        let rose = high && self.pins & bit == 0;
        if high {
            self.pins |= bit;
        } else {
            self.pins &= !bit;
        }
        if self.has(source, con::LEVEL) {
            if high {
                self.request |= source.bit();
            } else {
                self.request &= !source.bit();
            }
        } else if rose {
            self.request |= source.bit();
        }
    }

    fn mask_register(&self) -> u16 {
        SOURCES
            .into_iter()
            .filter(|&s| self.has(s, con::MASK))
            .fold(0, |mask, s| mask | s.bit())
    }

    fn set_mask_register(&mut self, value: u16) {
        for s in SOURCES {
            let control = &mut self.control[s.index()];
            *control = (*control & !con::MASK) | if value & s.bit() != 0 { con::MASK } else { 0 };
        }
    }

    fn raise_timer(&mut self, n: u8) {
        self.status |= 1 << n;
        self.request |= Source::Timer.bit();
    }
}

/// A timer's mode/control bits.
mod tcon {
    pub const CONTINUOUS: u16 = 0x0001;
    pub const ALTERNATE: u16 = 0x0002;
    pub const EXTERNAL: u16 = 0x0004;
    pub const PRESCALE: u16 = 0x0008;
    pub const RETRIGGER: u16 = 0x0010;
    pub const MAX_COUNT: u16 = 0x0020;
    pub const REGISTER_B: u16 = 0x1000;
    pub const INTERRUPT: u16 = 0x2000;
    pub const INHIBIT: u16 = 0x4000;
    pub const ENABLE: u16 = 0x8000;
}

#[derive(Clone, Copy, Debug)]
struct Timer {
    count: u16,
    compare: [u16; 2],
    control: u16,
    /// Timers 0 and 1 have a B register, an input pin and a prescaler.
    full: bool,
}

impl Timer {
    const fn new(full: bool) -> Self {
        Self {
            count: 0,
            compare: [0; 2],
            control: 0,
            full,
        }
    }

    const fn enabled(&self) -> bool {
        self.control & tcon::ENABLE != 0
    }

    /// Whether the timer counts timer 2's maxima (`prescaled`) or the
    /// internal clock. With its input pin unwired an external clock never
    /// ticks, and a gate tied high always passes.
    const fn counts(&self, prescaled: bool) -> bool {
        self.enabled()
            && self.control & tcon::EXTERNAL == 0
            && (self.control & tcon::PRESCALE != 0) == prescaled
    }

    const fn dual(&self) -> bool {
        self.full && self.control & tcon::ALTERNATE != 0
    }

    fn compare_in_use(&self) -> u16 {
        self.compare[usize::from(self.control & tcon::REGISTER_B != 0)]
    }

    /// Clocks until the count, incremented then compared, next equals the
    /// register in use; a count past it wraps through `FFFFh` first.
    fn until_max(&self) -> u64 {
        u64::from(
            self.compare_in_use()
                .wrapping_sub(self.count)
                .wrapping_sub(1),
        ) + 1
    }

    /// Clocks one full cycle takes: both registers in dual mode.
    fn cycle(&self) -> u64 {
        let length = |c: u16| if c == 0 { 0x1_0000 } else { u64::from(c) };
        if self.dual() {
            length(self.compare[0]) + length(self.compare[1])
        } else {
            length(self.compare_in_use())
        }
    }

    /// Counts `clocks`; returns how many maxima it reached.
    fn advance(&mut self, mut clocks: u64) -> u64 {
        let mut reached = 0;
        while clocks > 0 && self.enabled() {
            let to_max = self.until_max();
            if clocks < to_max {
                self.count = low16(u32::from(self.count) + u32::try_from(clocks).unwrap_or(0));
                break;
            }
            clocks -= to_max;
            self.count = 0;
            self.control |= tcon::MAX_COUNT;
            reached += 1;
            let finished = !self.dual() || self.control & tcon::REGISTER_B != 0;
            if self.dual() {
                self.control ^= tcon::REGISTER_B;
            }
            let continuous = self.control & tcon::CONTINUOUS != 0;
            if finished && !continuous {
                self.control &= !tcon::ENABLE;
            } else if continuous && clocks >= self.cycle() {
                // Whole cycles from a count of zero change only the tally.
                let cycles = clocks / self.cycle();
                clocks %= self.cycle();
                reached += cycles * if self.dual() { 2 } else { 1 };
            }
        }
        reached
    }

    /// Clocks until the `n`th maximum from now, if the timer gets there.
    fn until_maxima(&self, n: u64) -> Option<u64> {
        if !self.enabled() || n == 0 {
            return None;
        }
        let first = self.until_max();
        if n == 1 {
            return Some(first);
        }
        let continuous = self.control & tcon::CONTINUOUS != 0;
        if !continuous {
            return (self.dual() && self.control & tcon::REGISTER_B == 0 && n == 2).then(|| {
                let b = self.compare[1];
                first + if b == 0 { 0x1_0000 } else { u64::from(b) }
            });
        }
        let mut probe = *self;
        probe.advance(first);
        let rest = n - 1;
        let pairs = if probe.dual() { rest / 2 } else { rest };
        let odd = probe.dual() && rest % 2 == 1;
        let next = if odd { probe.until_max() } else { 0 };
        Some(first + pairs * probe.cycle() + next)
    }

    fn write_control(&mut self, value: u16) {
        let enable = if value & tcon::INHIBIT != 0 {
            value & tcon::ENABLE
        } else {
            self.control & tcon::ENABLE
        };
        let options = if self.full {
            tcon::ALTERNATE | tcon::EXTERNAL | tcon::PRESCALE | tcon::RETRIGGER
        } else {
            0
        };
        let writable = tcon::CONTINUOUS | options | tcon::MAX_COUNT | tcon::INTERRUPT;
        let in_use = if value & tcon::ALTERNATE != 0 && self.full {
            self.control & tcon::REGISTER_B
        } else {
            0
        };
        self.control = (value & writable) | in_use | enable;
    }
}

/// A DMA channel's control bits.
mod dcon {
    pub const WORD: u16 = 0x0001;
    pub const START: u16 = 0x0002;
    pub const CHANGE: u16 = 0x0004;
    pub const TIMER_REQUEST: u16 = 0x0010;
    pub const PRIORITY: u16 = 0x0020;
    pub const SYNC: u16 = 0x00C0;
    pub const INTERRUPT: u16 = 0x0100;
    pub const STOP_AT_COUNT: u16 = 0x0200;
    pub const SRC_INC: u16 = 0x0400;
    pub const SRC_DEC: u16 = 0x0800;
    pub const SRC_MEMORY: u16 = 0x1000;
    pub const DST_INC: u16 = 0x2000;
    pub const DST_DEC: u16 = 0x4000;
    pub const DST_MEMORY: u16 = 0x8000;
}

#[derive(Clone, Copy, Debug, Default)]
struct DmaChannel {
    source: u32,
    destination: u32,
    count: u16,
    control: u16,
    /// Timer 2 maxima not yet answered with a transfer.
    timer_requests: u64,
}

impl DmaChannel {
    const fn has(&self, bit: u16) -> bool {
        self.control & bit != 0
    }

    /// Whether the channel would transfer now.
    const fn wants(&self) -> bool {
        self.has(dcon::START)
            && if self.has(dcon::TIMER_REQUEST) {
                self.timer_requests > 0
            } else {
                self.control & dcon::SYNC == 0
            }
    }
}

/// The address after a transfer of `width` bytes: incrementing, decrementing,
/// or held when both or neither is asked for.
fn stepped(addr: u32, width: u32, inc: bool, dec: bool) -> u32 {
    match (inc, dec) {
        (true, false) => addr.wrapping_add(width) & 0xF_FFFF,
        (false, true) => addr.wrapping_sub(width) & 0xF_FFFF,
        _ => addr,
    }
}

/// The memory and I/O an integrated DMA transfer reaches.
pub trait DmaBus {
    /// Reads a byte of memory.
    fn memory_read(&mut self, addr: u32) -> u8;
    /// Writes a byte of memory.
    fn memory_write(&mut self, addr: u32, value: u8);
    /// Reads an I/O port.
    fn io_read(&mut self, port: u16) -> u8;
    /// Writes an I/O port.
    fn io_write(&mut self, port: u16, value: u8);
}

/// What the chip-select unit adds to an access in one of its ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChipWaits {
    states: u8,
    with_ready: bool,
}

impl ChipWaits {
    fn of(register: u16) -> Self {
        Self {
            states: lo(register) & 3,
            with_ready: register & 4 == 0,
        }
    }

    /// The wait states of an access the board itself would hold for `board`
    /// states: the unit's own, and the board's too unless told to ignore
    /// READY.
    #[must_use]
    pub fn with_board(self, board: u8) -> u8 {
        if self.with_ready {
            self.states.max(board)
        } else {
            self.states
        }
    }

    fn merge(a: Option<Self>, b: Self) -> Self {
        a.map_or(b, |a| Self {
            states: a.states.max(b.states),
            with_ready: a.with_ready || b.with_ready,
        })
    }
}

/// Which chip-select register is which.
mod cs {
    pub const UMCS: usize = 0;
    pub const LMCS: usize = 1;
    pub const PACS: usize = 2;
    pub const MMCS: usize = 3;
    pub const MPCS: usize = 4;
}

/// What a register write changed that the board must act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Nothing beyond the block itself.
    None,
    /// The block moved, or between I/O and memory space.
    Relocated,
    /// A chip-select range or its wait states changed.
    ChipSelects,
}

/// Where the block is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    /// At an I/O port.
    Io(u16),
    /// At a physical memory address.
    Memory(u32),
}

/// The relocation register's ESC-trap bit.
const RELOCATION_ET: u16 = 0x8000;
/// The relocation register's slave-mode bit.
const RELOCATION_SLAVE: u16 = 0x4000;
/// The relocation register's memory-space bit.
const RELOCATION_MEMORY: u16 = 0x1000;
/// The relocation register as RESET leaves it: I/O space at `FF00h`.
const RELOCATION_RESET: u16 = 0x20FF;
/// The upper memory chip select as RESET leaves it: the top kilobyte, with
/// three wait states and READY honoured.
const UMCS_RESET: u16 = 0xFFFB;

/// Transfers a channel makes in one visit before the CPU gets the bus, so
/// one visit's work is bounded however long a transfer runs.
const DMA_BURST: u32 = 0x1_0000;
/// Clocks in a bus cycle.
const BUS_CYCLE: u64 = 4;

/// The integrated peripherals.
#[derive(Clone, Debug)]
pub struct Pcb {
    relocation: u16,
    icu: Icu,
    /// The slave-mode interrupt registers, which drive nothing on this board.
    slave: [u16; 16],
    timers: [Timer; 3],
    dma: [DmaChannel; 2],
    /// Which channel won the last tie between equal priorities.
    dma_last: usize,
    chip_selects: [u16; 5],
    /// Which of LMCS, PACS, MMCS and MPCS have been written, which makes
    /// their ranges live.
    programmed: u8,
    bus8: bool,
    /// Timer clocks already applied.
    clocks: u64,
    /// Bus clocks DMA took from the CPU since last asked.
    stolen: u64,
}

impl Pcb {
    /// The block as RESET leaves it, for a CPU with an 8-bit (80188) or
    /// 16-bit (80186) bus.
    #[must_use]
    pub const fn new(bus8: bool) -> Self {
        Self {
            relocation: RELOCATION_RESET,
            icu: Icu::new(),
            slave: [0; 16],
            timers: [Timer::new(true), Timer::new(true), Timer::new(false)],
            dma: [DmaChannel {
                source: 0,
                destination: 0,
                count: 0,
                control: 0,
                timer_requests: 0,
            }; 2],
            dma_last: 1,
            chip_selects: [UMCS_RESET, 0, 0, 0, 0],
            programmed: 0,
            bus8,
            clocks: 0,
            stolen: 0,
        }
    }

    /// Where the block is.
    #[must_use]
    pub fn space(&self) -> Space {
        let base = u32::from(self.relocation & 0x0FFF) << 8;
        if self.relocation & RELOCATION_MEMORY != 0 {
            Space::Memory(base)
        } else {
            Space::Io(low16(base))
        }
    }

    /// Whether ESC instructions trap to interrupt 7.
    #[must_use]
    pub const fn escape_traps(&self) -> bool {
        self.relocation & RELOCATION_ET != 0
    }

    /// Whether the controller is in slave mode, which leaves the 8259A to
    /// interrupt the CPU directly.
    #[must_use]
    pub const fn slave_mode(&self) -> bool {
        self.relocation & RELOCATION_SLAVE != 0
    }

    /// Whether the controller asks the CPU for an interrupt.
    #[must_use]
    pub fn intr(&self) -> bool {
        !self.slave_mode() && self.icu.pending().is_some()
    }

    /// The interrupt-acknowledge cycles; `None` if nothing is pending.
    pub fn acknowledge(&mut self) -> Option<Ack> {
        if self.slave_mode() {
            return None;
        }
        let source = self.icu.pending()?;
        Some(self.icu.take(source))
    }

    /// Drives external interrupt pin INT0–INT3.
    pub fn set_int_pin(&mut self, n: u8, high: bool) {
        self.icu.set_pin(n, high);
    }

    /// An IRET completed: DMA resumes after a halt.
    pub fn interrupt_returned(&mut self) {
        self.icu.status &= !DMA_HALT;
    }

    /// The CPU took NMI: DMA halts until the IRET.
    pub fn nmi_taken(&mut self) {
        self.icu.status |= DMA_HALT;
    }

    /// Bus clocks DMA took from the CPU since the last call.
    pub fn take_stolen(&mut self) -> u64 {
        core::mem::take(&mut self.stolen)
    }

    /// Advances the timers to timer clock `clocks` (the CPU's clock over
    /// four), raising their interrupts and timer 2's DMA requests.
    pub fn advance(&mut self, clocks: u64) {
        let elapsed = clocks.saturating_sub(self.clocks);
        self.clocks = self.clocks.max(clocks);
        if elapsed == 0 {
            return;
        }
        let prescale = if self.timers[2].counts(false) {
            self.timers[2].advance(elapsed)
        } else {
            0
        };
        if prescale > 0 {
            if self.timers[2].control & tcon::INTERRUPT != 0 {
                self.icu.raise_timer(2);
            }
            for channel in &mut self.dma {
                if channel.has(dcon::TIMER_REQUEST) {
                    channel.timer_requests = channel.timer_requests.saturating_add(prescale);
                }
            }
        }
        for n in 0..2u8 {
            let timer = &mut self.timers[usize::from(n)];
            let ticks = if timer.counts(true) {
                prescale
            } else if timer.counts(false) {
                elapsed
            } else {
                0
            };
            if timer.advance(ticks) > 0 && timer.control & tcon::INTERRUPT != 0 {
                self.icu.raise_timer(n);
            }
        }
    }

    /// The timer clock at which a timer next interrupts or timer 2 next
    /// requests DMA, if either will.
    #[must_use]
    pub fn next_event(&self) -> Option<u64> {
        let t2 = &self.timers[2];
        let interrupts = |t: &Timer| t.control & tcon::INTERRUPT != 0;
        let paces = self
            .dma
            .iter()
            .any(|c| c.has(dcon::START) && c.has(dcon::TIMER_REQUEST));
        let mut next = (t2.counts(false) && (interrupts(t2) || paces)).then(|| t2.until_max());
        for timer in &self.timers[..2] {
            if !interrupts(timer) {
                continue;
            }
            let at = if timer.counts(false) {
                Some(timer.until_max())
            } else if timer.counts(true) && t2.counts(false) {
                t2.until_maxima(timer.until_max())
            } else {
                None
            };
            next = match (next, at) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        next.map(|d| self.clocks + d)
    }

    /// Whether a channel has transfers waiting for the bus.
    #[must_use]
    pub fn dma_pending(&self) -> bool {
        self.icu.status & DMA_HALT == 0 && self.dma.iter().any(DmaChannel::wants)
    }

    /// The unit's own clocks counted, from the first.
    #[must_use]
    pub const fn clocks(&self) -> u64 {
        self.clocks
    }

    /// The bus cycles its DMA has taken and not yet charged.
    #[must_use]
    pub const fn stolen(&self) -> u64 {
        self.stolen
    }

    /// Runs the transfers the channels want now, up to a burst.
    pub fn run_dma<B: DmaBus>(&mut self, bus: &mut B) {
        for _ in 0..DMA_BURST {
            if !self.dma_pending() {
                return;
            }
            let n = self.arbitrate();
            self.transfer(n, bus);
        }
    }

    /// The channel that takes the next transfer: the higher priority, or by
    /// turns between equals.
    fn arbitrate(&mut self) -> usize {
        let wants = [self.dma[0].wants(), self.dma[1].wants()];
        let n = match wants {
            [true, false] => 0,
            [false, true] => 1,
            _ => match (
                self.dma[0].has(dcon::PRIORITY),
                self.dma[1].has(dcon::PRIORITY),
            ) {
                (true, false) => 0,
                (false, true) => 1,
                _ => 1 - self.dma_last,
            },
        };
        self.dma_last = n;
        n
    }

    fn transfer<B: DmaBus>(&mut self, n: usize, bus: &mut B) {
        let channel = self.dma[n];
        let word_transfer = channel.has(dcon::WORD);
        let width: u32 = if word_transfer { 2 } else { 1 };
        for i in 0..width {
            let src = channel.source.wrapping_add(i) & 0xF_FFFF;
            let dst = channel.destination.wrapping_add(i) & 0xF_FFFF;
            let byte = if channel.has(dcon::SRC_MEMORY) {
                bus.memory_read(src)
            } else {
                bus.io_read(low16(src))
            };
            if channel.has(dcon::DST_MEMORY) {
                bus.memory_write(dst, byte);
            } else {
                bus.io_write(low16(dst), byte);
            }
        }
        let cycles = |addr: u32| {
            if word_transfer && (self.bus8 || addr & 1 != 0) {
                2
            } else {
                1
            }
        };
        self.stolen += BUS_CYCLE * (cycles(channel.source) + cycles(channel.destination));
        let c = &mut self.dma[n];
        c.source = stepped(c.source, width, c.has(dcon::SRC_INC), c.has(dcon::SRC_DEC));
        c.destination = stepped(
            c.destination,
            width,
            c.has(dcon::DST_INC),
            c.has(dcon::DST_DEC),
        );
        c.count = c.count.wrapping_sub(1);
        if c.has(dcon::TIMER_REQUEST) {
            c.timer_requests -= 1;
        }
        if c.count == 0 && c.has(dcon::STOP_AT_COUNT) {
            c.control &= !dcon::START;
            c.timer_requests = 0;
            if c.has(dcon::INTERRUPT) {
                self.icu.request |= [Source::Dma0, Source::Dma1][n].bit();
            }
        }
    }

    /// The chip-select unit's wait states for the memory byte at `addr`,
    /// where a range covers it. Overlapping ranges take the larger count.
    #[must_use]
    pub fn memory_waits(&self, addr: u32) -> Option<ChipWaits> {
        let r = &self.chip_selects;
        let mut waits = None;
        let mut hit =
            |register: u16| waits = Some(ChipWaits::merge(waits, ChipWaits::of(register)));
        if addr >= u32::from(r[cs::UMCS] & 0xFFC0) << 4 {
            hit(r[cs::UMCS]);
        }
        if self.is_programmed(cs::LMCS) && addr < (u32::from(r[cs::LMCS] & 0x3FC0) + 0x40) << 4 {
            hit(r[cs::LMCS]);
        }
        if self.is_programmed(cs::MMCS) && self.is_programmed(cs::MPCS) {
            let base = u32::from(r[cs::MMCS] & 0xFE00) << 4;
            let size = u32::from((r[cs::MPCS] >> 8) & 0x7F) << 13;
            if addr.wrapping_sub(base) < size {
                hit(r[cs::MMCS]);
            }
        }
        if self.pcs_in_memory() {
            if let Some(register) = self.peripheral(addr) {
                hit(register);
            }
        }
        waits
    }

    /// The chip-select unit's wait states for I/O port `port`, where a
    /// peripheral chip select covers it.
    #[must_use]
    pub fn io_waits(&self, port: u16) -> Option<ChipWaits> {
        if self.pcs_in_memory() {
            return None;
        }
        self.peripheral(u32::from(port)).map(ChipWaits::of)
    }

    /// The smallest span, in bytes and aligned to itself, within which the
    /// unit's answer for memory can change: a peripheral select's 128 bytes
    /// when they are in memory space, else a kilobyte.
    #[must_use]
    pub fn memory_granule(&self) -> u32 {
        if self.pcs_in_memory() {
            128
        } else {
            1024
        }
    }

    fn is_programmed(&self, register: usize) -> bool {
        self.programmed & (1 << register) != 0
    }

    fn pcs_in_memory(&self) -> bool {
        self.is_programmed(cs::PACS)
            && self.is_programmed(cs::MPCS)
            && self.chip_selects[cs::MPCS] & 0x40 != 0
    }

    /// The register whose waits govern the peripheral select covering
    /// `addr`: PACS for PCS0–3, MPCS for PCS4–6, where PCS5 and PCS6 are
    /// selects only when MPCS asks.
    fn peripheral(&self, addr: u32) -> Option<u16> {
        if !self.is_programmed(cs::PACS) || !self.is_programmed(cs::MPCS) {
            return None;
        }
        let r = &self.chip_selects;
        let base = u32::from(r[cs::PACS] & 0xFFC0) << 4;
        let base = if self.pcs_in_memory() {
            base
        } else {
            base & 0xFFFF
        };
        let select = addr.wrapping_sub(base) / 128;
        let selects = if r[cs::MPCS] & 0x80 != 0 { 7 } else { 5 };
        match select {
            0..=3 => Some(r[cs::PACS]),
            s if s < selects => Some(r[cs::MPCS]),
            _ => None,
        }
    }

    /// Reads the word register at `offset` within the block. Reading the
    /// poll register acknowledges the interrupt it reports.
    pub fn read(&mut self, offset: u8) -> u16 {
        if offset & 0xFE == 0x24 && !self.slave_mode() {
            if let Some(source) = self.icu.pending() {
                self.icu.take(source);
                return 0x8000 | u16::from(self.icu.vector_of(source));
            }
        }
        self.value(offset)
    }

    /// The word register at `offset`, as a read with no side effect sees it.
    fn value(&self, offset: u8) -> u16 {
        let offset = offset & 0xFE;
        match offset {
            0x20..=0x3E if self.slave_mode() => self.slave[usize::from((offset - 0x20) / 2)],
            0x24 | 0x26 => self
                .icu
                .pending()
                .map_or(0, |s| 0x8000 | u16::from(self.icu.vector_of(s))),
            0x28 => self.icu.mask_register(),
            0x2A => self.icu.priority_mask,
            0x2C => self.icu.in_service,
            0x2E => self.icu.request,
            0x30 => self.icu.status,
            0x32..=0x3E => self.icu.control[usize::from((offset - 0x32) / 2)],
            0x50..=0x66 => self.read_timer(offset),
            0xA0..=0xA8 => self.chip_selects[usize::from((offset - 0xA0) / 2)],
            0xC0..=0xCA | 0xD0..=0xDA => self.read_dma(offset),
            0xFE => self.relocation,
            _ => 0,
        }
    }

    /// Reads the byte at `offset`: one half of its word register.
    pub fn read_byte(&mut self, offset: u8) -> u8 {
        half(self.read(offset & 0xFE), offset)
    }

    /// The byte at `offset` without a read's side effects.
    #[must_use]
    pub fn peek_byte(&self, offset: u8) -> u8 {
        half(self.value(offset), offset)
    }

    fn read_timer(&self, offset: u8) -> u16 {
        let (n, register) = timer_register(offset);
        let timer = &self.timers[n];
        match register {
            0 => timer.count,
            1 => timer.compare[0],
            2 if timer.full => timer.compare[1],
            3 => timer.control,
            _ => 0,
        }
    }

    fn read_dma(&self, offset: u8) -> u16 {
        let channel = &self.dma[usize::from(offset >= 0xD0)];
        match offset & 0x0F {
            0x0 => low16(channel.source),
            0x2 => high16(channel.source),
            0x4 => low16(channel.destination),
            0x6 => high16(channel.destination),
            0x8 => channel.count,
            _ => channel.control,
        }
    }

    /// Writes the word register at `offset` within the block.
    pub fn write(&mut self, offset: u8, value: u16) -> Effect {
        let offset = offset & 0xFE;
        match offset {
            0x20..=0x3E if self.slave_mode() => {
                self.slave[usize::from((offset - 0x20) / 2)] = value;
            }
            0x22 => self.icu.eoi(value),
            0x28 => self.icu.set_mask_register(value),
            0x2A => self.icu.priority_mask = value & con::PRIORITY,
            0x2C => self.icu.in_service = value & 0xFD,
            0x2E => {
                let dma = Source::Dma0.bit() | Source::Dma1.bit();
                self.icu.request = (self.icu.request & !dma) | (value & dma);
            }
            0x30 => {
                self.icu.status = value & (DMA_HALT | 7);
                if self.icu.timer() == 0 {
                    self.icu.request &= !Source::Timer.bit();
                } else {
                    self.icu.request |= Source::Timer.bit();
                }
            }
            0x32..=0x3E => {
                let index = usize::from((offset - 0x32) / 2);
                let writable = match index {
                    0..=2 => con::PRIORITY | con::MASK,
                    3 | 4 => con::PRIORITY | con::MASK | con::LEVEL | con::CASCADE | con::NESTED,
                    _ => con::PRIORITY | con::MASK | con::LEVEL,
                };
                self.icu.control[index] = value & writable;
            }
            0x50..=0x66 => self.write_timer(offset, value),
            0xA0..=0xA8 => {
                let register = usize::from((offset - 0xA0) / 2);
                self.chip_selects[register] = value;
                self.programmed |= 1 << register;
                return Effect::ChipSelects;
            }
            0xC0..=0xCA | 0xD0..=0xDA => self.write_dma(offset, value),
            0xFE => {
                // Bit 13 is unused and reads as one.
                self.relocation = (value & !0x2000) | 0x2000;
                return Effect::Relocated;
            }
            _ => {}
        }
        Effect::None
    }

    /// Writes the byte at `offset` into its half of the word register,
    /// keeping the other half's value.
    pub fn write_byte(&mut self, offset: u8, value: u8) -> Effect {
        let current = self.value(offset & 0xFE);
        let merged = if offset & 1 == 0 {
            word(value, crate::bits::hi(current))
        } else {
            word(lo(current), value)
        };
        self.write(offset & 0xFE, merged)
    }

    fn write_timer(&mut self, offset: u8, value: u16) {
        let (n, register) = timer_register(offset);
        let timer = &mut self.timers[n];
        match register {
            0 => timer.count = value,
            1 => timer.compare[0] = value,
            2 if timer.full => timer.compare[1] = value,
            3 => timer.write_control(value),
            _ => {}
        }
    }

    fn write_dma(&mut self, offset: u8, value: u16) {
        let channel = &mut self.dma[usize::from(offset >= 0xD0)];
        let high = u32::from(value & 0xF) << 16;
        match offset & 0x0F {
            0x0 => channel.source = (channel.source & 0xF_0000) | u32::from(value),
            0x2 => channel.source = (channel.source & 0xFFFF) | high,
            0x4 => channel.destination = (channel.destination & 0xF_0000) | u32::from(value),
            0x6 => channel.destination = (channel.destination & 0xFFFF) | high,
            0x8 => channel.count = value,
            _ => {
                let start = if value & dcon::CHANGE != 0 {
                    value & dcon::START
                } else {
                    channel.control & dcon::START
                };
                channel.control = (value & !(dcon::START | dcon::CHANGE | 0x0008)) | start;
                if !channel.has(dcon::TIMER_REQUEST) || start == 0 {
                    channel.timer_requests = 0;
                }
            }
        }
    }
}

/// The half of `register` the byte at `offset` is.
fn half(register: u16, offset: u8) -> u8 {
    if offset & 1 == 0 {
        lo(register)
    } else {
        crate::bits::hi(register)
    }
}

/// Which timer and which of its registers an offset names.
fn timer_register(offset: u8) -> (usize, u8) {
    let index = offset - 0x50;
    (usize::from(index / 8).min(2), index % 8 / 2)
}

crate::snapshot::state!(Icu {
    control,
    priority_mask,
    in_service,
    request,
    status,
    pins,
});

crate::snapshot::state!(Timer {
    count,
    compare,
    control,
    full,
});

crate::snapshot::state!(DmaChannel {
    source,
    destination,
    count,
    control,
    timer_requests,
});

crate::snapshot::state!(
    Pcb {
        relocation,
        icu,
        slave,
        timers,
        dma,
        dma_last,
        chip_selects,
        programmed,
        clocks,
        stolen,
    }
    as built { bus8 }
    check |pcb: &Pcb| pcb.dma_last < pcb.dma.len()
);

#[cfg(test)]
#[path = "pcb186_tests.rs"]
mod tests;
