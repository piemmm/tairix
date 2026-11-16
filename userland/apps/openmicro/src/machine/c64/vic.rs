//! The VIC-II, 6569 (PAL) and 6567R8 (NTSC): its memory accesses at their
//! cycles, the display and idle states, the sprites, the border unit, and
//! the raster, collision and light-pen interrupts, drawn into the canvas.
//!
//! A cycle is taken in two steps around the CPU's half of it:
//! [`Vic::begin_cycle`] makes the first half's access and decides BA, and
//! [`Vic::end_cycle`] makes the second half's and draws the cycle's eight
//! pixels with the registers as the CPU left them. Which access falls in
//! which half of which cycle is the chip's timing table, from Christian
//! Bauer's VIC-II article and, for the 6567R8's sprite cycles, VICE's
//! `vicii-chip-model.c`.

use alloc::vec;
use alloc::vec::Vec;

use crate::machine::canvas::{Canvas, DirtyRows, CANVAS_LINES, CANVAS_WIDTH};
use crate::snapshot;

/// Registers, repeated through the chip's page every 64 bytes.
pub mod reg {
    /// Sprite 0's X coordinate, low eight bits; sprite `n`'s at `2n`.
    pub const SPRITE_X: u8 = 0x00;
    /// Sprite 0's Y coordinate; sprite `n`'s at `2n + 1`.
    pub const SPRITE_Y: u8 = 0x01;
    /// Bit 8 of each sprite's X coordinate.
    pub const SPRITE_X_HIGH: u8 = 0x10;
    /// Control 1: vertical scroll, rows, display enable, modes, raster bit 8.
    pub const CR1: u8 = 0x11;
    /// The raster line read; the line to interrupt at written.
    pub const RASTER: u8 = 0x12;
    /// The light pen's X coordinate, halved.
    pub const LPX: u8 = 0x13;
    /// The light pen's line.
    pub const LPY: u8 = 0x14;
    /// Sprites enabled.
    pub const SPRITE_ENABLE: u8 = 0x15;
    /// Control 2: horizontal scroll, columns, multicolour.
    pub const CR2: u8 = 0x16;
    /// Sprites expanded vertically.
    pub const SPRITE_EXPAND_Y: u8 = 0x17;
    /// The video matrix and character base addresses.
    pub const MEMORY: u8 = 0x18;
    /// Interrupt causes latched; written ones clear.
    pub const IRQ: u8 = 0x19;
    /// Interrupt causes enabled.
    pub const IRQ_ENABLE: u8 = 0x1A;
    /// Sprites behind the foreground.
    pub const SPRITE_PRIORITY: u8 = 0x1B;
    /// Sprites in multicolour.
    pub const SPRITE_MULTICOLOUR: u8 = 0x1C;
    /// Sprites expanded horizontally.
    pub const SPRITE_EXPAND_X: u8 = 0x1D;
    /// Sprites that met a sprite; reading clears.
    pub const SPRITE_SPRITE: u8 = 0x1E;
    /// Sprites that met the foreground; reading clears.
    pub const SPRITE_DATA: u8 = 0x1F;
    /// The border's colour.
    pub const BORDER: u8 = 0x20;
    /// Background colour 0; colours 1 to 3 follow.
    pub const BACKGROUND: u8 = 0x21;
    /// Sprite multicolour 0; multicolour 1 follows.
    pub const SPRITE_MULTI: u8 = 0x25;
    /// Sprite 0's colour; the other sprites' follow.
    pub const SPRITE_COLOUR: u8 = 0x27;
}

/// Interrupt causes, as the latch's bits.
pub mod irq {
    /// The raster reached the line written.
    pub const RASTER: u8 = 0x01;
    /// A sprite met the foreground.
    pub const SPRITE_DATA: u8 = 0x02;
    /// Sprites met.
    pub const SPRITE_SPRITE: u8 = 0x04;
    /// The light pen fired.
    pub const LIGHT_PEN: u8 = 0x08;
    /// Every cause.
    pub const CAUSES: u8 = 0x0F;
    /// Read: an enabled cause is latched.
    pub const ANY: u8 = 0x80;
}

/// Control register 1's bits.
pub mod cr1 {
    /// The vertical scroll.
    pub const YSCROLL: u8 = 0x07;
    /// 25 rows rather than 24.
    pub const RSEL: u8 = 0x08;
    /// The display is enabled.
    pub const DEN: u8 = 0x10;
    /// Bitmap mode.
    pub const BMM: u8 = 0x20;
    /// Extended colour mode.
    pub const ECM: u8 = 0x40;
    /// Bit 8 of the raster line.
    pub const RST8: u8 = 0x80;
}

/// Control register 2's bits.
pub mod cr2 {
    /// The horizontal scroll.
    pub const XSCROLL: u8 = 0x07;
    /// 40 columns rather than 38.
    pub const CSEL: u8 = 0x08;
    /// Multicolour mode.
    pub const MCM: u8 = 0x10;
    /// Bits that read back as written; the rest read set.
    pub const WRITTEN: u8 = 0x3F;
}

/// Which chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// The PAL 6569: 312 lines of 63 cycles.
    Mos6569,
    /// The NTSC 6567R8: 263 lines of 65 cycles.
    Mos6567R8,
}

impl Model {
    /// Cycles a line.
    #[must_use]
    pub const fn cycles(self) -> u16 {
        match self {
            Self::Mos6569 => 63,
            Self::Mos6567R8 => 65,
        }
    }

    /// Lines a frame.
    #[must_use]
    pub const fn lines(self) -> u16 {
        match self {
            Self::Mos6569 => 312,
            Self::Mos6567R8 => 263,
        }
    }

    /// Machine cycles a second: the chip's dot clock over eight.
    #[must_use]
    pub const fn clock(self) -> u64 {
        match self {
            Self::Mos6569 => 985_248,
            Self::Mos6567R8 => 1_022_727,
        }
    }

    /// The cycle sprite 0's pointer is read in; the others' follow two
    /// apart, and the sprite checks keep their place before it.
    const fn sprite_zero(self) -> usize {
        match self {
            Self::Mos6569 => 58,
            Self::Mos6567R8 => 59,
        }
    }

    /// The X coordinate a half-cycle begins at, counting halves from the
    /// first of cycle 1.
    fn half_x(self, half: usize) -> u16 {
        let x = match self {
            Self::Mos6569 => (404 + 4 * half) % 504,
            Self::Mos6567R8 => {
                // The counter holds at $184 through three halves of cycles 62
                // and 63, fitting 520 pixels to 512 places.
                let counted = if half <= 122 {
                    half
                } else if half <= 124 {
                    122
                } else {
                    half - 2
                };
                (412 + 4 * counted) % 512
            }
        };
        u16::try_from(x).unwrap_or(u16::MAX)
    }

    /// The first line the picture shows and how many it does.
    const fn visible_lines(self) -> (u16, u16) {
        match self {
            Self::Mos6569 => (16, 284),
            Self::Mos6567R8 => (41, 235),
        }
    }

    /// The first pixel of a line the picture shows, counting from cycle 1's
    /// first, and how many it does.
    const fn visible_pixels(self) -> (u16, u16) {
        match self {
            Self::Mos6569 => (76, 405),
            Self::Mos6567R8 => (77, 428),
        }
    }

    const fn table(self) -> &'static Table {
        match self {
            Self::Mos6569 => &PAL,
            Self::Mos6567R8 => &NTSC,
        }
    }

    const fn number(self) -> u8 {
        match self {
            Self::Mos6569 => 0,
            Self::Mos6567R8 => 1,
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
        match r.get::<u8>()? {
            0 => Ok(Self::Mos6569),
            1 => Ok(Self::Mos6567R8),
            _ => Err(snapshot::SnapshotError::Malformed),
        }
    }
}

/// What the chip reads in its half of each cycle: its 16 KiB of memory and
/// colour RAM.
pub trait VicMemory {
    /// The byte at 14-bit address `addr` of the chip's bank.
    fn fetch(&self, addr: u16) -> u8;
    /// Colour RAM's four bits at `addr`'s low ten.
    fn colour(&self, addr: u16) -> u8;
}

/// The access in a cycle's first half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum First {
    /// A read discarded.
    Idle,
    /// A sprite's data pointer.
    Pointer(u8),
    /// A sprite's second data byte, if its DMA is on.
    Sprite(u8),
    /// A DRAM refresh.
    Refresh,
    /// The graphics byte.
    Graphics,
}

/// The access in a cycle's second half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Second {
    /// The CPU's.
    Cpu,
    /// A sprite's first or third data byte, if its DMA is on.
    Sprite(u8),
    /// The video matrix and colour RAM, on a bad line.
    Matrix,
}

/// What happens to the display logic in a cycle.
mod flag {
    pub const UPDATE_VC: u8 = 0x01;
    pub const CRUNCH: u8 = 0x02;
    pub const UPDATE_MCBASE: u8 = 0x04;
    pub const SPRITE_DMA: u8 = 0x08;
    pub const SPRITE_EXPAND: u8 = 0x10;
    pub const SPRITE_DISPLAY: u8 = 0x20;
    pub const UPDATE_RC: u8 = 0x40;
    pub const VERTICAL_BORDER: u8 = 0x80;
}

/// One cycle of the timing table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    first: First,
    second: Second,
    /// Sprites whose DMA holds BA low through this cycle.
    sprites_ba: u8,
    /// A bad line holds BA low through this cycle.
    fetch_ba: bool,
    flags: u8,
}

const MAX_CYCLES: usize = 65;

/// A model's cycles, indexed from 1.
type Table = [Slot; MAX_CYCLES + 1];

const PAL: Table = table(Model::Mos6569);
const NTSC: Table = table(Model::Mos6567R8);

/// The cycle `steps` on from `cycle`, wrapping into the next line.
const fn cycle_after(model: Model, cycle: usize, steps: usize) -> usize {
    let cycles = model.cycles() as usize;
    (cycle - 1 + steps) % cycles + 1
}

const fn table(model: Model) -> Table {
    let idle = Slot {
        first: First::Idle,
        second: Second::Cpu,
        sprites_ba: 0,
        fetch_ba: false,
        flags: 0,
    };
    let mut t = [idle; MAX_CYCLES + 1];
    let mut cycle = 11;
    while cycle <= 55 {
        t[cycle].first = if cycle <= 15 {
            First::Refresh
        } else {
            First::Graphics
        };
        if cycle >= 15 && cycle <= 54 {
            t[cycle].second = Second::Matrix;
        }
        if cycle >= 12 && cycle <= 54 {
            t[cycle].fetch_ba = true;
        }
        cycle += 1;
    }
    let cycles = model.cycles() as usize;
    let zero = model.sprite_zero();
    let mut sprite = 0;
    while sprite < 8 {
        let pointer = cycle_after(model, zero, 2 * sprite as usize);
        let next = cycle_after(model, pointer, 1);
        t[pointer].first = First::Pointer(sprite);
        t[pointer].second = Second::Sprite(sprite);
        t[next].first = First::Sprite(sprite);
        t[next].second = Second::Sprite(sprite);
        // BA falls three cycles before the first data byte's half.
        let mut step = 0;
        while step < 5 {
            let held = cycle_after(model, pointer, cycles - 3 + step);
            t[held].sprites_ba |= 1 << sprite;
            step += 1;
        }
        sprite += 1;
    }
    t[14].flags |= flag::UPDATE_VC;
    t[15].flags |= flag::CRUNCH;
    t[16].flags |= flag::UPDATE_MCBASE;
    t[zero - 3].flags |= flag::SPRITE_DMA;
    t[zero - 2].flags |= flag::SPRITE_DMA;
    t[56].flags |= flag::SPRITE_EXPAND;
    t[zero].flags |= flag::SPRITE_DISPLAY;
    t[58].flags |= flag::UPDATE_RC;
    t[63].flags |= flag::VERTICAL_BORDER;
    t
}

/// A sprite's DMA and display logic and its data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent flip-flop of the sprite's logic"
)]
struct Sprite {
    pointer: u8,
    mc: u8,
    mcbase: u8,
    dma: bool,
    display: bool,
    /// The flip-flop that lets a line advance: held set unless the sprite
    /// is expanded vertically, and toggled each line while it is.
    advance: bool,
    /// Its vertical expansion was cleared in cycle 15 with the flip-flop
    /// clear: cycle 16 blends MC into MCBASE rather than copying it.
    crunch: bool,
    /// The three bytes the data accesses read, high first.
    data: u32,
    /// The bytes being shown and the pixels shown of them, from the X match.
    shifter: u32,
    shown: Option<u8>,
}

snapshot::fields!(
    Sprite {
        pointer,
        mc,
        mcbase,
        dma,
        display,
        advance,
        crunch,
        data,
        shifter,
        shown
    },
    check | s | s.mc < 64 && s.mcbase < 64 && s.data < 1 << 24 && s.shown.is_none_or(|p| p <= 48)
);

/// The address the idle state's accesses read: the bank's last byte.
const IDLE_ADDRESS: u16 = 0x3FFF;

/// A graphics byte fetched, waiting for the pixel it is loaded at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Load {
    /// The pixel of the line, from cycle 1's first.
    at: u16,
    byte: u8,
    /// The video matrix byte and its colour, as bits 0–7 and 8–11.
    matrix: u16,
}

snapshot::fields!(Load { at, byte, matrix });

/// The graphics sequencer: the byte being shifted out, its matrix data,
/// and the bytes fetched but not yet loaded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Graphics {
    pending: [Option<Load>; 2],
    shifter: u8,
    matrix: u16,
    /// The bits a multicolour pixel pair shows.
    pair: u8,
    /// The pair's second pixel is next.
    second_half: bool,
}

snapshot::fields!(Graphics {
    pending,
    shifter,
    matrix,
    pair,
    second_half
});

/// The picture's place on the canvas.
const fn margin(available: usize, used: u16) -> usize {
    (available - used as usize) / 2
}

/// A VIC-II.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent latch or flip-flop of the silicon"
)]
pub struct Vic {
    model: Model,
    sprite_x: [u16; 8],
    sprite_y: [u8; 8],
    cr1: u8,
    compare: u16,
    light_pen: [u8; 2],
    sprite_enable: u8,
    cr2: u8,
    expand_y: u8,
    memory: u8,
    irq_latch: u8,
    irq_enable: u8,
    priority: u8,
    multicolour: u8,
    expand_x: u8,
    sprite_sprite: u8,
    sprite_data: u8,
    border: u8,
    background: [u8; 4],
    sprite_multi: [u8; 2],
    sprite_colour: [u8; 8],
    /// The beam: its line and its cycle in it, from 1.
    line: u16,
    cycle: u16,
    /// The raster register, a cycle behind the beam at line 0.
    raster: u16,
    /// The raster interrupt has been raised in this line.
    raster_raised: bool,
    /// DEN was set in a cycle of line $30: bad lines are allowed.
    den_seen: bool,
    /// Display state rather than idle.
    display: bool,
    /// The bad line condition holds this cycle.
    bad: bool,
    vc: u16,
    vcbase: u16,
    rc: u8,
    vmli: u8,
    /// The video matrix line: each column's matrix byte and colour.
    matrix: [u16; 40],
    refresh: u8,
    /// BA has been low this many cycles running, to four.
    ba_cycles: u8,
    /// The chip takes this cycle's second half from the CPU.
    dma: bool,
    /// What this cycle's first-half access read.
    phi1: u8,
    sprites: [Sprite; 8],
    /// Sprites whose shifters have started this line.
    started: u8,
    graphics: Graphics,
    main_border: bool,
    vertical_border: bool,
    /// The light pen input's level, and whether it has fired this frame.
    light_pen_level: bool,
    light_pen_fired: bool,
    canvas: Canvas,
    /// The visible part of the line being drawn, two slots a pixel.
    row: Vec<u8>,
}

/// The longest visible row any model draws, in slots.
const MAX_ROW: usize = 2 * 428;

snapshot::fields!(
    Vic {
        model,
        sprite_x,
        sprite_y,
        cr1,
        compare,
        light_pen,
        sprite_enable,
        cr2,
        expand_y,
        memory,
        irq_latch,
        irq_enable,
        priority,
        multicolour,
        expand_x,
        sprite_sprite,
        sprite_data,
        border,
        background,
        sprite_multi,
        sprite_colour,
        line,
        cycle,
        raster,
        raster_raised,
        den_seen,
        display,
        bad,
        vc,
        vcbase,
        rc,
        vmli,
        matrix,
        refresh,
        ba_cycles,
        dma,
        phi1,
        sprites,
        started,
        graphics,
        main_border,
        vertical_border,
        light_pen_level,
        light_pen_fired,
        canvas,
        row <= MAX_ROW
    },
    check | v, r | v.consistent() && v.beam() == beam_at(v.model, r.clock())
);

/// The beam's line and cycle once the first half of machine cycle `cycle`
/// is taken, the first beginning a frame.
fn beam_at(model: Model, cycle: u64) -> (u16, u16) {
    let cycles = u64::from(model.cycles());
    let in_frame = cycle % (cycles * u64::from(model.lines()));
    let line = u16::try_from(in_frame / cycles).unwrap_or(u16::MAX);
    let cycle = u16::try_from(in_frame % cycles).unwrap_or(u16::MAX) + 1;
    (line, cycle)
}

impl Vic {
    /// A chip at power-on, about to begin a frame.
    #[must_use]
    pub fn new(model: Model) -> Self {
        let (_, pixels) = model.visible_pixels();
        Self {
            model,
            sprite_x: [0; 8],
            sprite_y: [0; 8],
            cr1: 0,
            compare: 0,
            light_pen: [0; 2],
            sprite_enable: 0,
            cr2: 0,
            expand_y: 0,
            memory: 0,
            irq_latch: 0,
            irq_enable: 0,
            priority: 0,
            multicolour: 0,
            expand_x: 0,
            sprite_sprite: 0,
            sprite_data: 0,
            border: 0,
            background: [0; 4],
            sprite_multi: [0; 2],
            sprite_colour: [0; 8],
            line: model.lines() - 1,
            cycle: model.cycles(),
            raster: model.lines() - 1,
            raster_raised: false,
            den_seen: false,
            display: false,
            bad: false,
            vc: 0,
            vcbase: 0,
            rc: 0,
            vmli: 0,
            matrix: [0; 40],
            refresh: 0xFF,
            ba_cycles: 0,
            dma: false,
            phi1: 0xFF,
            sprites: [Sprite::default(); 8],
            started: 0,
            graphics: Graphics::default(),
            main_border: true,
            vertical_border: true,
            light_pen_level: true,
            light_pen_fired: false,
            canvas: Canvas::filled(0),
            row: vec![0; 2 * usize::from(pixels)],
        }
    }

    /// Whether a restored chip's state is one the chip can be in.
    fn consistent(&self) -> bool {
        let (_, pixels) = self.model.visible_pixels();
        self.line < self.model.lines()
            && (1..=self.model.cycles()).contains(&self.cycle)
            && self.raster < self.model.lines()
            && self.compare < 0x200
            && self.sprite_x.iter().all(|&x| x < 0x200)
            && self.vc < 0x400
            && self.vcbase < 0x400
            && self.rc < 8
            && usize::from(self.vmli) <= self.matrix.len()
            && self.matrix.iter().all(|&m| m < 0x1000)
            && self.ba_cycles <= 4
            && self.row.len() == 2 * usize::from(pixels)
            && self.cr2 & !cr2::WRITTEN == 0
            && self.irq_latch & !irq::CAUSES == 0
            && self.irq_enable & !irq::CAUSES == 0
            && self
                .graphics
                .pending
                .iter()
                .flatten()
                .all(|load| load.matrix < 0x1000)
    }

    /// Which chip it is.
    #[must_use]
    pub const fn model(&self) -> Model {
        self.model
    }

    /// The beam's line and its cycle in it, from 1.
    #[must_use]
    pub const fn beam(&self) -> (u16, u16) {
        (self.line, self.cycle)
    }

    /// The beam's line.
    #[must_use]
    pub const fn line(&self) -> u16 {
        self.line
    }

    /// The beam's cycle in its line, from 1.
    #[must_use]
    pub const fn cycle(&self) -> u16 {
        self.cycle
    }

    /// Whether BA is low this cycle: a CPU read waits.
    #[must_use]
    pub const fn ba(&self) -> bool {
        self.ba_cycles > 0
    }

    /// Whether the chip takes this cycle's second half: the CPU's address
    /// lines are off the bus.
    #[must_use]
    pub const fn dma(&self) -> bool {
        self.dma
    }

    /// The byte this cycle's first-half access read, which an undriven read
    /// in the second half finds still on the bus.
    #[must_use]
    pub const fn phi1(&self) -> u8 {
        self.phi1
    }

    /// The interrupt line, asserted.
    #[must_use]
    pub const fn irq(&self) -> bool {
        self.irq_latch & self.irq_enable != 0
    }

    /// Frames finished since power-on.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.canvas.frames()
    }

    /// The last finished frame.
    #[must_use]
    pub fn shown(&self) -> &[u8] {
        self.canvas.shown()
    }

    /// The rows of the finished frame that changed since last asked.
    pub fn take_dirty_rows(&mut self) -> DirtyRows {
        self.canvas.take_dirty()
    }

    fn table_slot(&self) -> Slot {
        self.model.table()[usize::from(self.cycle)]
    }

    /// The bad line condition, as the chip samples it as a cycle begins.
    fn bad_line(&self) -> bool {
        self.den_seen
            && (0x30..=0xF7).contains(&self.raster)
            && self.raster & 7 == u16::from(self.cr1 & cr1::YSCROLL)
    }

    /// Raise the raster interrupt if the raster is at the line written and
    /// it has not been raised in this line.
    fn compare_raster(&mut self) {
        if self.raster == self.compare && !self.raster_raised {
            self.irq_latch |= irq::RASTER;
            self.raster_raised = true;
        }
    }

    /// The first half of the next cycle: the beam moves on, the display
    /// logic takes the cycle's steps, BA is decided and the first-half
    /// access is made.
    pub fn begin_cycle<M: VicMemory>(&mut self, memory: &M) {
        self.cycle += 1;
        if self.cycle > self.model.cycles() {
            self.cycle = 1;
            self.line += 1;
            if self.line == self.model.lines() {
                self.line = 0;
                self.vcbase = 0;
                self.refresh = 0xFF;
                self.den_seen = false;
                self.light_pen_fired = false;
            }
            self.started = 0;
            self.graphics.pending = [None; 2];
        }
        // The raster register and its interrupt move on a cycle late at line 0.
        if self.cycle == if self.line == 0 { 2 } else { 1 } {
            self.raster = self.line;
            self.raster_raised = false;
            self.compare_raster();
        }
        if self.line == 0x30 && self.cr1 & cr1::DEN != 0 {
            self.den_seen = true;
        }
        self.bad = self.bad_line();
        if self.bad {
            self.display = true;
        }
        let slot = self.table_slot();
        if slot.flags != 0 {
            self.step_display_logic(slot.flags);
        }
        let ba = (slot.fetch_ba && self.bad)
            || (slot.sprites_ba != 0 && slot.sprites_ba & self.sprite_dma() != 0);
        self.ba_cycles = if ba { (self.ba_cycles + 1).min(4) } else { 0 };
        self.dma = match slot.second {
            Second::Matrix => self.bad && self.ba_cycles == 4,
            Second::Sprite(n) => self.sprites[usize::from(n)].dma,
            Second::Cpu => false,
        };
        self.phi1 = self.first_access(slot.first, memory);
    }

    /// The sprites whose DMA is on.
    fn sprite_dma(&self) -> u8 {
        self.sprites
            .iter()
            .enumerate()
            .filter(|(_, sprite)| sprite.dma)
            .fold(0, |mask, (n, _)| mask | 1 << n)
    }

    fn step_display_logic(&mut self, flags: u8) {
        let raster = self.raster.to_le_bytes()[0];
        if flags & flag::UPDATE_VC != 0 {
            self.vc = self.vcbase;
            self.vmli = 0;
            if self.bad {
                self.rc = 0;
            }
        }
        if flags & flag::UPDATE_MCBASE != 0 {
            for sprite in &mut self.sprites {
                if sprite.crunch {
                    sprite.mcbase = (0b10_1010 & sprite.mcbase & sprite.mc)
                        | (0b01_0101 & (sprite.mcbase | sprite.mc));
                    sprite.crunch = false;
                } else if sprite.advance {
                    sprite.mcbase = sprite.mc;
                }
                if sprite.mcbase == 63 {
                    sprite.dma = false;
                }
            }
        }
        if flags & flag::SPRITE_DMA != 0 {
            for (n, sprite) in self.sprites.iter_mut().enumerate() {
                if self.sprite_enable & 1 << n != 0 && self.sprite_y[n] == raster && !sprite.dma {
                    sprite.dma = true;
                    sprite.mcbase = 0;
                    sprite.advance = true;
                }
            }
        }
        if flags & flag::SPRITE_EXPAND != 0 {
            for (n, sprite) in self.sprites.iter_mut().enumerate() {
                if self.expand_y & 1 << n != 0 && sprite.dma {
                    sprite.advance = !sprite.advance;
                }
            }
        }
        if flags & flag::SPRITE_DISPLAY != 0 {
            for (n, sprite) in self.sprites.iter_mut().enumerate() {
                sprite.mc = sprite.mcbase;
                if !sprite.dma {
                    sprite.display = false;
                } else if self.sprite_y[n] == raster {
                    sprite.display = true;
                }
            }
        }
        if flags & flag::UPDATE_RC != 0 {
            if self.rc == 7 {
                self.vcbase = self.vc;
                if !self.bad {
                    self.display = false;
                }
            }
            if self.display {
                self.rc = (self.rc + 1) & 7;
            }
        }
        if flags & flag::VERTICAL_BORDER != 0 {
            self.check_vertical_border();
        }
    }

    /// The vertical border's comparisons against the line.
    fn check_vertical_border(&mut self) {
        let (top, bottom) = if self.cr1 & cr1::RSEL != 0 {
            (51, 251)
        } else {
            (55, 247)
        };
        if self.line == bottom {
            self.vertical_border = true;
        }
        if self.line == top && self.cr1 & cr1::DEN != 0 {
            self.vertical_border = false;
        }
    }

    fn first_access<M: VicMemory>(&mut self, first: First, memory: &M) -> u8 {
        match first {
            First::Idle => memory.fetch(IDLE_ADDRESS),
            First::Pointer(n) => {
                let byte = memory.fetch(self.matrix_base() | 0x3F8 | u16::from(n));
                self.sprites[usize::from(n)].pointer = byte;
                byte
            }
            First::Sprite(n) => self
                .sprite_access(n, 1, memory)
                .unwrap_or_else(|| memory.fetch(IDLE_ADDRESS)),
            First::Refresh => {
                let byte = memory.fetch(0x3F00 | u16::from(self.refresh));
                self.refresh = self.refresh.wrapping_sub(1);
                byte
            }
            First::Graphics => self.graphics_access(memory),
        }
    }

    const fn matrix_base(&self) -> u16 {
        ((self.memory as u16) >> 4) << 10
    }

    /// Where the character generator lies in the bank.
    #[must_use]
    pub const fn character_base(&self) -> u16 {
        ((self.memory & 0x0E) as u16) << 10
    }

    /// Sprite `n`'s data byte `index` read, if its DMA is on.
    fn sprite_access<M: VicMemory>(&mut self, n: u8, index: u8, memory: &M) -> Option<u8> {
        let sprite = &mut self.sprites[usize::from(n)];
        if !sprite.dma {
            return None;
        }
        let byte = memory.fetch(u16::from(sprite.pointer) << 6 | u16::from(sprite.mc));
        sprite.mc = (sprite.mc + 1) & 63;
        let shift = 8 * (2 - u32::from(index));
        sprite.data = sprite.data & !(0xFF << shift) | u32::from(byte) << shift;
        Some(byte)
    }

    /// The g-access: in display state the graphics for the matrix byte at
    /// VMLI, in idle state the last byte of the bank.
    fn graphics_access<M: VicMemory>(&mut self, memory: &M) -> u8 {
        let ecm = self.cr1 & cr1::ECM != 0;
        let (addr, matrix) = if self.display {
            let matrix = self
                .matrix
                .get(usize::from(self.vmli))
                .copied()
                .unwrap_or_default();
            let addr = if self.cr1 & cr1::BMM != 0 {
                u16::from(self.memory & 0x08) << 10 | self.vc << 3 | u16::from(self.rc)
            } else {
                self.character_base() | (matrix & 0xFF) << 3 | u16::from(self.rc)
            };
            self.vc = (self.vc + 1) & 0x3FF;
            self.vmli += 1;
            (addr, matrix)
        } else {
            (IDLE_ADDRESS, 0)
        };
        // Extended colour mode holds address lines 9 and 10 low.
        let addr = if ecm { addr & !0x0600 } else { addr };
        let byte = memory.fetch(addr);
        let at = (self.cycle - 1) * 8 + 4 + u16::from(self.cr2 & cr2::XSCROLL);
        let load = Some(Load { at, byte, matrix });
        let slot = self.graphics.pending.iter_mut().find(|p| p.is_none());
        match slot {
            Some(free) => *free = load,
            None => self.graphics.pending[1] = load,
        }
        byte
    }

    /// The second half of the cycle, `bus` holding what the CPU's access in
    /// it carried: the chip's own access, then the cycle's pixels.
    pub fn end_cycle<M: VicMemory>(&mut self, memory: &M, bus: u8) {
        match self.table_slot().second {
            Second::Matrix if self.bad => {
                // BA has not been low three cycles: the chip does not have
                // the bus, and takes the data lines as they float.
                let data = if self.dma {
                    let addr = self.matrix_base() | self.vc;
                    u16::from(memory.fetch(addr)) | u16::from(memory.colour(self.vc) & 0x0F) << 8
                } else {
                    0xFF | u16::from(bus & 0x0F) << 8
                };
                if let Some(cell) = self.matrix.get_mut(usize::from(self.vmli)) {
                    *cell = data;
                }
            }
            Second::Sprite(n) => {
                let index = if self.table_slot().first == First::Pointer(n) {
                    0
                } else {
                    2
                };
                let _ = self.sprite_access(n, index, memory);
            }
            Second::Matrix | Second::Cpu => {}
        }
        self.draw();
        if self.cycle == self.model.cycles() {
            self.end_line();
        }
    }

    /// The sprites that can show a pixel in this cycle: those shifting out,
    /// and those displayed that have not started in this line.
    fn live_sprites(&self) -> u8 {
        self.sprites
            .iter()
            .enumerate()
            .filter(|&(n, sprite)| {
                sprite.shown.is_some() || (sprite.display && self.started & 1 << n == 0)
            })
            .fold(0, |live, (n, _)| live | 1 << n)
    }

    fn draw(&mut self) {
        let (first, count) = self.model.visible_pixels();
        let csel = self.cr2 & cr2::CSEL != 0;
        let (left, right) = if csel { (24, 344) } else { (31, 335) };
        let base = (self.cycle - 1) * 8;
        let half = 2 * (usize::from(self.cycle) - 1);
        let half_x = [self.model.half_x(half), self.model.half_x(half + 1)];
        let window = base..base + 8;
        let loading = self
            .graphics
            .pending
            .iter()
            .flatten()
            .any(|load| window.contains(&load.at));
        let sprites = self.live_sprites();
        for pixel in 0..8 {
            let beam = base + pixel;
            let x = half_x[usize::from(pixel / 4)] + pixel % 4;
            if loading {
                for slot in &mut self.graphics.pending {
                    if let Some(load) = slot.filter(|load| load.at == beam) {
                        self.graphics.shifter = load.byte;
                        self.graphics.matrix = load.matrix;
                        self.graphics.second_half = false;
                        *slot = None;
                    }
                }
            }
            if x == right {
                self.main_border = true;
            }
            if x == left {
                self.check_vertical_border();
                if !self.vertical_border {
                    self.main_border = false;
                }
            }
            let colour = self.pixel(x, sprites);
            if let Some(column) = beam.checked_sub(first).filter(|&c| c < count) {
                let at = 2 * usize::from(column);
                self.row[at] = colour;
                self.row[at + 1] = colour;
            }
        }
    }

    /// The colour of the pixel at `x`, with its collisions, `sprites` the
    /// ones that can show there.
    fn pixel(&mut self, x: u16, sprites: u8) -> u8 {
        let (graphics, foreground) = self.graphics_pixel();
        let mut present = 0u8;
        let mut top = None;
        let mut rest = sprites;
        while rest != 0 {
            let n = rest.trailing_zeros() as usize;
            rest &= rest - 1;
            if let Some(colour) = self.sprite_pixel(n, x) {
                present |= 1 << n;
                top = top.or(Some((n, colour)));
            }
        }
        if present & present.wrapping_sub(1) != 0 {
            if self.sprite_sprite == 0 {
                self.irq_latch |= irq::SPRITE_SPRITE;
            }
            self.sprite_sprite |= present;
        }
        if foreground && present != 0 {
            if self.sprite_data == 0 {
                self.irq_latch |= irq::SPRITE_DATA;
            }
            self.sprite_data |= present;
        }
        if self.main_border {
            return self.border;
        }
        match top {
            Some((n, colour)) if !(foreground && self.priority & 1 << n != 0) => colour,
            _ => graphics,
        }
    }

    /// Sprite `n`'s colour at `x`, if it shows one there.
    fn sprite_pixel(&mut self, n: usize, x: u16) -> Option<u8> {
        let bit = 1 << n;
        let sprite = &mut self.sprites[n];
        if sprite.shown.is_none()
            && sprite.display
            && self.started & bit == 0
            && self.sprite_x[n] == x
        {
            self.started |= bit;
            sprite.shifter = sprite.data;
            sprite.shown = Some(0);
        }
        let shown = sprite.shown?;
        let expanded = self.expand_x & bit != 0;
        let index = if expanded { shown / 2 } else { shown };
        if index >= 24 {
            sprite.shown = None;
            return None;
        }
        sprite.shown = Some(shown + 1);
        let bits = if self.multicolour & bit != 0 {
            (sprite.shifter >> (22 - 2 * u32::from(index / 2))) & 3
        } else {
            ((sprite.shifter >> (23 - u32::from(index))) & 1) << 1
        };
        match bits {
            1 => Some(self.sprite_multi[0]),
            2 => Some(self.sprite_colour[n]),
            3 => Some(self.sprite_multi[1]),
            _ => None,
        }
    }

    /// The graphics sequencer's next pixel: its colour, and whether it
    /// counts as foreground for priority and collisions.
    fn graphics_pixel(&mut self) -> (u8, bool) {
        let ecm = self.cr1 & cr1::ECM != 0;
        let bmm = self.cr1 & cr1::BMM != 0;
        let mcm = self.cr2 & cr2::MCM != 0;
        let g = &mut self.graphics;
        let [char, colour] = g.matrix.to_le_bytes();
        let colour = colour & 0x0F;
        let multicolour = mcm && (bmm || colour & 0x08 != 0);
        let bits = if multicolour {
            if !g.second_half {
                g.pair = g.shifter >> 6;
                g.shifter <<= 2;
            }
            g.pair
        } else {
            let bit = g.shifter >> 7;
            g.shifter <<= 1;
            bit
        };
        g.second_half = !g.second_half;
        if self.vertical_border {
            return (self.background[0], false);
        }
        let background = |index: u8| self.background[usize::from(index)];
        match (ecm, bmm, mcm) {
            (false, false, false) => text(bits, colour, background(0)),
            (false, false, true) if colour & 0x08 == 0 => text(bits, colour & 0x07, background(0)),
            (false, false, true) => match bits {
                0 | 1 => (background(bits), false),
                2 => (background(2), true),
                _ => (colour & 0x07, true),
            },
            (false, true, false) => {
                if bits != 0 {
                    (char >> 4, true)
                } else {
                    (char & 0x0F, false)
                }
            }
            (false, true, true) => match bits {
                0 => (background(0), false),
                1 => (char >> 4, false),
                2 => (char & 0x0F, true),
                _ => (colour, true),
            },
            (true, false, false) => text(bits, colour, background(char >> 6)),
            // The invalid modes draw black, keeping their foreground.
            (true, false, true) => (0, if multicolour { bits >= 2 } else { bits != 0 }),
            (true, true, false) => (0, bits != 0),
            (true, true, true) => (0, bits >= 2),
        }
    }

    /// The line is drawn: paint it, and finish the frame after its last
    /// visible line.
    fn end_line(&mut self) {
        let lines = self.model.lines();
        let (first, count) = self.model.visible_lines();
        let shown = (self.line + lines - first) % lines;
        if shown < count {
            let line = margin(CANVAS_LINES, count) + usize::from(shown);
            let (_, pixels) = self.model.visible_pixels();
            let column = margin(CANVAS_WIDTH, 2 * pixels);
            self.canvas.paint(2 * line, column, &self.row);
            self.canvas.paint(2 * line + 1, column, &self.row);
            if shown == count - 1 {
                self.canvas.finish();
            }
        }
    }

    /// The CPU reads `register` (its low six bits).
    pub fn read(&mut self, register: u8) -> u8 {
        let index = usize::from(register & 0x3F);
        match register & 0x3F {
            r @ 0x00..=0x0F if r & 1 == 0 => self.sprite_x[index / 2].to_le_bytes()[0],
            0x00..=0x0F => self.sprite_y[index / 2],
            reg::SPRITE_X_HIGH => {
                (0..8).fold(0, |high, n| high | u8::from(self.sprite_x[n] >= 0x100) << n)
            }
            reg::CR1 => self.cr1 & !cr1::RST8 | u8::from(self.raster >= 0x100) << 7,
            reg::RASTER => self.raster.to_le_bytes()[0],
            reg::LPX => self.light_pen[0],
            reg::LPY => self.light_pen[1],
            reg::SPRITE_ENABLE => self.sprite_enable,
            reg::CR2 => self.cr2 | !cr2::WRITTEN,
            reg::SPRITE_EXPAND_Y => self.expand_y,
            reg::MEMORY => self.memory | 0x01,
            reg::IRQ => self.irq_latch | 0x70 | if self.irq() { irq::ANY } else { 0 },
            reg::IRQ_ENABLE => self.irq_enable | 0xF0,
            reg::SPRITE_PRIORITY => self.priority,
            reg::SPRITE_MULTICOLOUR => self.multicolour,
            reg::SPRITE_EXPAND_X => self.expand_x,
            reg::SPRITE_SPRITE => core::mem::take(&mut self.sprite_sprite),
            reg::SPRITE_DATA => core::mem::take(&mut self.sprite_data),
            reg::BORDER => self.border | 0xF0,
            0x21..=0x24 => self.background[index - 0x21] | 0xF0,
            0x25..=0x26 => self.sprite_multi[index - 0x25] | 0xF0,
            0x27..=0x2E => self.sprite_colour[index - 0x27] | 0xF0,
            _ => 0xFF,
        }
    }

    /// The CPU writes `value` to `register` (its low six bits).
    pub fn write(&mut self, register: u8, value: u8) {
        let index = usize::from(register & 0x3F);
        match register & 0x3F {
            r @ 0x00..=0x0F if r & 1 == 0 => {
                let x = &mut self.sprite_x[index / 2];
                *x = *x & 0x100 | u16::from(value);
            }
            0x00..=0x0F => self.sprite_y[index / 2] = value,
            reg::SPRITE_X_HIGH => {
                for (n, x) in self.sprite_x.iter_mut().enumerate() {
                    *x = *x & 0xFF | u16::from(value >> n & 1) << 8;
                }
            }
            reg::CR1 => {
                self.cr1 = value;
                self.compare = self.compare & 0xFF | u16::from(value & cr1::RST8) << 1;
                if self.line == 0x30 && value & cr1::DEN != 0 {
                    self.den_seen = true;
                }
                self.compare_raster();
            }
            reg::RASTER => {
                self.compare = self.compare & 0x100 | u16::from(value);
                self.compare_raster();
            }
            reg::SPRITE_ENABLE => self.sprite_enable = value,
            reg::CR2 => self.cr2 = value & cr2::WRITTEN,
            reg::SPRITE_EXPAND_Y => self.write_expand_y(value),
            reg::MEMORY => self.memory = value & 0xFE,
            reg::IRQ => self.irq_latch &= !(value & irq::CAUSES),
            reg::IRQ_ENABLE => self.irq_enable = value & irq::CAUSES,
            reg::SPRITE_PRIORITY => self.priority = value,
            reg::SPRITE_MULTICOLOUR => self.multicolour = value,
            reg::SPRITE_EXPAND_X => self.expand_x = value,
            reg::BORDER => self.border = value & 0x0F,
            0x21..=0x24 => self.background[index - 0x21] = value & 0x0F,
            0x25..=0x26 => self.sprite_multi[index - 0x25] = value & 0x0F,
            0x27..=0x2E => self.sprite_colour[index - 0x27] = value & 0x0F,
            _ => {}
        }
    }

    /// The flip-flop of a sprite whose vertical expansion is cleared is
    /// set; cleared in cycle 15 while the flip-flop was clear, the sprite
    /// crunches.
    fn write_expand_y(&mut self, value: u8) {
        let crunch_cycle = self.table_slot().flags & flag::CRUNCH != 0;
        for (n, sprite) in self.sprites.iter_mut().enumerate() {
            if value & 1 << n == 0 {
                if crunch_cycle && !sprite.advance {
                    sprite.crunch = true;
                }
                sprite.advance = true;
            }
        }
        self.expand_y = value;
    }

    /// The light pen input at `level` from the CPU's half of this cycle:
    /// falling, it latches the beam's position as the cycle ends, once a
    /// frame.
    pub fn set_light_pen(&mut self, level: bool) {
        if self.light_pen_level && !level && !self.light_pen_fired {
            let end = self.model.half_x(2 * usize::from(self.cycle));
            self.light_pen = [(end >> 1).to_le_bytes()[0], self.raster.to_le_bytes()[0]];
            self.irq_latch |= irq::LIGHT_PEN;
            self.light_pen_fired = true;
        }
        self.light_pen_level = level;
    }
}

/// A text-mode pixel: set, its own colour; clear, `background`.
const fn text(bits: u8, colour: u8, background: u8) -> (u8, bool) {
    if bits != 0 {
        (colour, true)
    } else {
        (background, false)
    }
}

#[cfg(test)]
#[path = "vic_tests.rs"]
mod tests;
