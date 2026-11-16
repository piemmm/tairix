//! The C64's memory as the CPU and the VIC-II reach it: 64 KiB of RAM, the
//! BASIC, KERNAL and character ROMs, colour RAM and the 6510's own port,
//! with the PLA choosing for each 4 KiB page which chip answers from the
//! port's LORAM, HIRAM and CHAREN and the expansion port's GAME and EXROM.

use alloc::boxed::Box;
use alloc::vec;

use tairix_crypto::FINGERPRINT_LEN;

use crate::machine::{Rom, RomError};
use crate::snapshot::{self, Reader, SnapshotError, Writer};

/// Bytes of RAM.
pub const RAM_BYTES: usize = 0x1_0000;
/// Bytes of the BASIC ROM, at `$A000`.
pub const BASIC_BYTES: usize = 0x2000;
/// Bytes of the KERNAL ROM, at `$E000`.
pub const KERNAL_BYTES: usize = 0x2000;
/// Bytes of the character ROM.
pub const CHARACTER_BYTES: usize = 0x1000;
/// The port lines with nothing on the board to hold them, each on its own:
/// what they last drove fades.
const FLOATING: [u8; 2] = [0x40, 0x80];
const _: () = assert!(FLOATING[0] | FLOATING[1] == line::UNCONNECTED);
/// Where the character ROM's second set, lower case with upper case, begins.
pub const LOWER_CASE_SET: usize = CHARACTER_BYTES / 2;
/// Colour RAM's cells, four bits each.
pub const COLOUR_CELLS: usize = 0x400;

/// Where the BASIC ROM answers.
pub const BASIC_BASE: u16 = 0xA000;
/// Where the KERNAL ROM answers.
pub const KERNAL_BASE: u16 = 0xE000;

const _: () = assert!(
    (BASIC_BASE as usize).is_multiple_of(BASIC_BYTES)
        && (KERNAL_BASE as usize).is_multiple_of(KERNAL_BYTES),
    "each ROM answers at a multiple of its size, so an address's low bits are its offset"
);

/// Cycles bits 6 and 7 of the port, unconnected, hold a 1 last driven once
/// they are inputs, as a 6510 averages.
pub const FALL_OFF_CYCLES: u64 = 350_000;

/// What answers the CPU at an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    /// RAM.
    Ram,
    /// The BASIC ROM.
    Basic,
    /// The KERNAL ROM.
    Kernal,
    /// The character ROM.
    Character,
    /// The I/O chips and colour RAM at `$D000–$DFFF`.
    Io,
    /// A cartridge's low ROM.
    RomLow,
    /// A cartridge's high ROM.
    RomHigh,
    /// Nothing inside the machine: the expansion port alone may answer.
    Open,
}

/// The port's lines the PLA reads, as bits of `$01`.
pub mod line {
    /// RAM or BASIC at `$A000`.
    pub const LORAM: u8 = 0x01;
    /// RAM or the KERNAL at `$E000`.
    pub const HIRAM: u8 = 0x02;
    /// The character ROM or I/O at `$D000`.
    pub const CHAREN: u8 = 0x04;
    /// The datasette's write line.
    pub const CASSETTE_WRITE: u8 = 0x08;
    /// The datasette's buttons, low while one is down.
    pub const CASSETTE_SENSE: u8 = 0x10;
    /// The datasette's motor, running while low.
    pub const CASSETTE_MOTOR: u8 = 0x20;
    /// The two unconnected lines.
    pub const UNCONNECTED: u8 = 0xC0;
    /// The three the PLA reads.
    pub const BANKING: u8 = LORAM | HIRAM | CHAREN;
}

/// The expansion port's two configuration lines, high unless a cartridge
/// grounds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cartridge {
    /// GAME.
    pub game: bool,
    /// EXROM.
    pub exrom: bool,
}

impl Cartridge {
    /// No cartridge: both lines high.
    pub const NONE: Self = Self {
        game: true,
        exrom: true,
    };

    /// Whether GAME alone is grounded: the Ultimax configuration.
    #[must_use]
    pub const fn ultimax(self) -> bool {
        !self.game && self.exrom
    }
}

/// The chip answering the CPU in 4 KiB page `page` with `banking`'s LORAM,
/// HIRAM and CHAREN and `cartridge`'s lines.
#[must_use]
pub const fn chip(banking: u8, cartridge: Cartridge, page: u8) -> Chip {
    let loram = banking & line::LORAM != 0;
    let hiram = banking & line::HIRAM != 0;
    let charen = banking & line::CHAREN != 0;
    let Cartridge { game, exrom } = cartridge;
    let ultimax = cartridge.ultimax();
    let roms = loram || hiram;
    match page {
        0x1..=0x7 | 0xA..=0xC if ultimax => Chip::Open,
        0x8 | 0x9 if ultimax || (!exrom && loram && hiram) => Chip::RomLow,
        0xA | 0xB if game && loram && hiram => Chip::Basic,
        0xA | 0xB if !game && !exrom && hiram => Chip::RomHigh,
        0xD if ultimax || (charen && roms) => Chip::Io,
        // A 16K cartridge's configurations keep the character ROM out unless
        // HIRAM is up.
        0xD if !charen && roms && (game || exrom || hiram) => Chip::Character,
        0xE | 0xF if ultimax => Chip::RomHigh,
        0xE | 0xF if hiram => Chip::Kernal,
        _ => Chip::Ram,
    }
}

/// Lines with pull-ups: the three the PLA reads and the datasette's sense.
const PULLED_UP: u8 = line::BANKING | line::CASSETTE_SENSE;

/// The 6510's own port: its direction register at `$00`, its data register
/// at `$01`, and the level each line was last driven to, which a line left
/// undriven with no pull-up keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Port {
    direction: u8,
    data: u8,
    latched: u8,
    /// The cycle lines 6 and 7, unconnected, lose their charge, line 6's
    /// first.
    falls: [u64; 2],
}

impl Port {
    /// The port at power-on: every line an input, so the pull-ups map the
    /// ROMs and I/O in.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            direction: 0,
            data: 0,
            latched: 0,
            falls: [0; 2],
        }
    }

    /// The levels the PLA sees: driven where an output, pulled up where not.
    #[must_use]
    pub const fn banking(&self) -> u8 {
        (self.data & self.direction | !self.direction) & line::BANKING
    }

    /// Whether the datasette's motor runs: its line low, driven so or left
    /// undriven for the motor switch's base resistor to pull down.
    #[must_use]
    pub const fn motor(&self) -> bool {
        self.data & self.direction & line::CASSETTE_MOTOR == 0
    }

    /// The datasette write line's level.
    #[must_use]
    pub const fn cassette_write(&self) -> bool {
        (self.data & self.direction | self.latched & !self.direction) & line::CASSETTE_WRITE != 0
    }

    /// Register `register` (0 or 1) read at cycle `now`, `sense` low while a
    /// datasette button is down.
    #[must_use]
    pub fn read(&self, register: u16, sense: bool, now: u64) -> u8 {
        if register & 1 == 0 {
            return self.direction;
        }
        // The motor switch's base resistor pulls its line low.
        let mut inputs = (self.latched | PULLED_UP) & !line::CASSETTE_MOTOR;
        if !sense {
            inputs &= !line::CASSETTE_SENSE;
        }
        for (bit, falls) in FLOATING.into_iter().zip(self.falls) {
            if now >= falls {
                inputs &= !bit;
            }
        }
        self.data & self.direction | inputs & !self.direction
    }

    /// `value` written to register `register` at cycle `now`. Lines 6 and 7
    /// keep the charge they were last driven to for [`FALL_OFF_CYCLES`] once
    /// they stop being driven.
    pub fn write(&mut self, register: u16, value: u8, now: u64) {
        if register & 1 == 0 {
            let released = self.direction & !value;
            for (index, bit) in FLOATING.into_iter().enumerate() {
                if released & bit != 0 {
                    self.falls[index] = now.saturating_add(FALL_OFF_CYCLES);
                }
            }
            self.direction = value;
        } else {
            self.data = value;
        }
        self.latched = self.latched & !self.direction | self.data & self.direction;
    }
}

snapshot::fields!(
    Port {
        direction,
        data,
        latched,
        falls
    },
    check | p,
    r | p.latched & p.direction == p.data & p.direction
        && p.falls
            .iter()
            .all(|&falls| falls <= r.clock() + FALL_OFF_CYCLES)
);

/// RAM, the three ROMs, colour RAM and the port.
#[derive(Clone, Debug)]
pub struct Memory {
    ram: Box<[u8]>,
    colour: Box<[u8]>,
    basic: Rom,
    kernal: Rom,
    character: Rom,
    /// The 6510's port.
    pub port: Port,
    /// The expansion port's configuration lines.
    pub cartridge: Cartridge,
}

/// `bytes` as a ROM of exactly `size` bytes.
fn rom_of(bytes: &[u8], size: usize) -> Result<Rom, RomError> {
    if bytes.len() != size {
        return Err(RomError::BadSize);
    }
    Ok(Rom::new(bytes.into()))
}

impl Memory {
    /// Memory holding these ROM images, RAM and colour RAM cleared.
    ///
    /// # Errors
    ///
    /// [`RomError::BadSize`] for an image not its ROM's size.
    pub fn new(basic: &[u8], kernal: &[u8], character: &[u8]) -> Result<Self, RomError> {
        Ok(Self {
            ram: vec![0; RAM_BYTES].into_boxed_slice(),
            colour: vec![0; COLOUR_CELLS].into_boxed_slice(),
            basic: rom_of(basic, BASIC_BYTES)?,
            kernal: rom_of(kernal, KERNAL_BYTES)?,
            character: rom_of(character, CHARACTER_BYTES)?,
            port: Port::new(),
            cartridge: Cartridge::NONE,
        })
    }

    /// The chip answering the CPU at `addr` now.
    #[must_use]
    pub fn cpu_chip(&self, addr: u16) -> Chip {
        chip(self.port.banking(), self.cartridge, (addr >> 12) as u8)
    }

    /// The byte `chip`, the one [`Self::cpu_chip`] names at `addr`, gives
    /// the CPU there: a ROM's, the character ROM's, or RAM's for the rest
    /// but the port and I/O.
    #[must_use]
    pub fn read_chip(&self, chip: Chip, addr: u16) -> u8 {
        let index = usize::from(addr);
        match chip {
            Chip::Basic => self.basic.image[index & (BASIC_BYTES - 1)],
            Chip::Kernal => self.kernal.image[index & (KERNAL_BYTES - 1)],
            Chip::Character => self.character.image[index & (CHARACTER_BYTES - 1)],
            Chip::Ram | Chip::Io | Chip::RomLow | Chip::RomHigh | Chip::Open => self.ram[index],
        }
    }

    /// RAM at `addr`, beneath whatever the CPU sees there.
    #[must_use]
    pub fn ram(&self, addr: u16) -> u8 {
        self.ram[usize::from(addr)]
    }

    /// `value` into RAM at `addr`: where a write to any chip but I/O and an
    /// Ultimax cartridge's lands.
    pub fn write_ram(&mut self, addr: u16, value: u8) {
        self.ram[usize::from(addr)] = value;
    }

    /// Colour RAM's cell at `addr`'s low ten bits, in the low nybble.
    #[must_use]
    pub fn colour(&self, addr: u16) -> u8 {
        self.colour[usize::from(addr) & (COLOUR_CELLS - 1)]
    }

    /// `value`'s low nybble into colour RAM at `addr`'s low ten bits.
    pub fn write_colour(&mut self, addr: u16, value: u8) {
        self.colour[usize::from(addr) & (COLOUR_CELLS - 1)] = value & 0x0F;
    }

    /// Record the ROMs by their fingerprints.
    pub fn save_roms(&self, w: &mut Writer) {
        for rom in [&self.basic, &self.kernal, &self.character] {
            w.put(&rom.fingerprint);
        }
    }

    /// Refuse a snapshot taken with other ROMs.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Fitting`] for another ROM, or the reader's error.
    pub fn check_roms(&self, r: &mut Reader<'_>) -> Result<(), SnapshotError> {
        for rom in [&self.basic, &self.kernal, &self.character] {
            if r.get::<[u8; FINGERPRINT_LEN]>()? != rom.fingerprint {
                return Err(SnapshotError::Fitting);
            }
        }
        Ok(())
    }

    /// Record RAM, colour RAM and the port.
    pub fn save_contents(&self, w: &mut Writer) {
        w.bytes(&self.ram);
        w.bytes(&self.colour);
        w.put(&self.port);
    }

    /// These ROMs with a snapshot's RAM, colour RAM and port; the
    /// expansion port's lines left for the cartridge to set.
    ///
    /// # Errors
    ///
    /// [`SnapshotError`] for contents no memory holds.
    pub fn restored(&self, r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        let ram = r.bytes_at_most(RAM_BYTES)?;
        let colour = r.bytes_at_most(COLOUR_CELLS)?;
        Reader::check(
            ram.len() == RAM_BYTES
                && colour.len() == COLOUR_CELLS
                && colour.iter().all(|&cell| cell <= 0x0F),
        )?;
        Ok(Self {
            ram: ram.into(),
            colour: colour.into(),
            basic: self.basic.clone(),
            kernal: self.kernal.clone(),
            character: self.character.clone(),
            port: r.get()?,
            cartridge: Cartridge::NONE,
        })
    }

    /// What the VIC-II fetches at its 14-bit address `addr` in bank `bank`:
    /// RAM, or the character ROM where [`vic_character_offset`] finds it.
    #[must_use]
    pub fn vic_read(&self, bank: u8, addr: u16) -> u8 {
        if let Some(offset) = vic_character_offset(bank, addr) {
            return self.character.image[offset];
        }
        self.ram[usize::from(vic_ram_address(bank, addr))]
    }
}

/// The RAM address the VIC-II's 14-bit address `addr` in bank `bank` names.
#[must_use]
#[inline]
pub const fn vic_ram_address(bank: u8, addr: u16) -> u16 {
    (bank as u16 & 3) << 14 | (addr & 0x3FFF)
}

/// The character ROM's byte the VIC-II reads at its 14-bit address `addr`
/// in bank `bank`, where it reads the ROM: `$1000–$1FFF` of banks 0 and 2.
#[must_use]
#[inline]
pub fn vic_character_offset(bank: u8, addr: u16) -> Option<usize> {
    let addr = addr & 0x3FFF;
    (bank & 1 == 0 && addr & 0x3000 == 0x1000).then(|| usize::from(addr) & (CHARACTER_BYTES - 1))
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
