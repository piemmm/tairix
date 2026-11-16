//! Cartridges on the expansion port: their ROMs as ROML at `$8000` and ROMH
//! at `$A000` (or `$E000` in the Ultimax configuration), the EXROM and GAME
//! lines through which they choose the PLA's mapping, and the banking each
//! hardware type does, through I/O1 and I/O2 or, on some, by reading its own
//! ROM (VICE's description of each type).

use alloc::vec;
use alloc::vec::Vec;

use super::memory;
use crate::media::crt::Crt;
use crate::snapshot::{self, Reader, SnapshotError, Writer};

/// Bytes in a ROML or ROMH bank.
pub const BANK_BYTES: usize = 0x2000;

/// The hardware types emulated, by their CRT numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 0: plain ROMs, mapped as the image's lines say.
    Generic,
    /// 4: Simons' BASIC, its upper half switched in and out at `$DE00`.
    SimonsBasic,
    /// 5: Ocean's, 8K banks chosen at `$DE00`, ROMH mirroring ROML.
    Ocean,
    /// 7: Fun Play and Power Play, 8K banks scrambled into `$DE00`.
    FunPlay,
    /// 8: Super Games, 16K banks chosen at `$DF00`.
    SuperGames,
    /// 10: Epyx's `FastLoad`, on while its capacitor stays discharged.
    EpyxFastLoad,
    /// 15: the C64 Game System and System 3's, the bank the low bits of
    /// any access to I/O1.
    System3,
    /// 17: Dinamic's, the bank the low bits of a read of I/O1.
    Dinamic,
    /// 18: Zaxxon's, a 4K ROML whose two halves choose ROMH's bank as they
    /// are read.
    Zaxxon,
    /// 19: Magic Desk, Domark and HES Australia, 8K banks chosen at `$DE00`.
    MagicDesk,
}

impl Kind {
    const ALL: [Self; 10] = [
        Self::Generic,
        Self::SimonsBasic,
        Self::Ocean,
        Self::FunPlay,
        Self::SuperGames,
        Self::EpyxFastLoad,
        Self::System3,
        Self::Dinamic,
        Self::Zaxxon,
        Self::MagicDesk,
    ];

    /// Its CRT hardware type.
    #[must_use]
    pub const fn hardware(self) -> u16 {
        match self {
            Self::Generic => 0,
            Self::SimonsBasic => 4,
            Self::Ocean => 5,
            Self::FunPlay => 7,
            Self::SuperGames => 8,
            Self::EpyxFastLoad => 10,
            Self::System3 => 15,
            Self::Dinamic => 17,
            Self::Zaxxon => 18,
            Self::MagicDesk => 19,
        }
    }

    /// The kind a CRT's hardware type names, if emulated.
    #[must_use]
    pub fn from_hardware(hardware: u16) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.hardware() == hardware)
    }
}

impl snapshot::Save for Kind {
    fn save(&self, w: &mut Writer) {
        w.put(&self.hardware());
    }
}

impl snapshot::Load for Kind {
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        Self::from_hardware(r.get()?).ok_or(SnapshotError::Malformed)
    }
}

/// Why a cartridge image was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CartridgeError {
    /// A hardware type not emulated.
    Unsupported(u16),
    /// A chip at an address or bank the type has no place for.
    Misplaced,
}

/// Cycles `FastLoad`'s capacitor takes to charge and switch its ROM off.
const EPYX_CHARGE_CYCLES: u64 = 512;

/// The most banks a cartridge has.
const MAX_BANKS: usize = 64;

/// A cartridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cartridge {
    kind: Kind,
    roml: Vec<Option<Vec<u8>>>,
    romh: Vec<Option<Vec<u8>>>,
    /// The banks' count less one, as a mask: Ocean's register and System 3's
    /// address reach no further.
    bank_mask: usize,
    bank: usize,
    /// ROMH's bank where it is chosen apart from ROML's.
    high_bank: usize,
    /// The lines as the cartridge holds them: true while low.
    exrom: bool,
    game: bool,
    /// The lines a generic cartridge's image starts it at.
    image_lines: (bool, bool),
    /// `FastLoad`: the cycle its ROM goes off unless read before.
    off_at: u64,
    /// Super Games: its register takes no more writes until reset.
    locked: bool,
}

/// Where a chip at `address` goes: ROML (`false`) or ROMH (`true`), and its
/// offset in the bank.
const fn slot(address: u16) -> Option<(bool, usize)> {
    match address {
        0x8000..=0x9FFF => Some((false, (address - 0x8000) as usize)),
        0xA000..=0xBFFF => Some((true, (address - 0xA000) as usize)),
        0xE000..=0xFFFF => Some((true, (address - 0xE000) as usize)),
        _ => None,
    }
}

impl Cartridge {
    /// The cartridge a CRT image holds, reset.
    ///
    /// # Errors
    ///
    /// [`CartridgeError::Unsupported`] for a hardware type not emulated, and
    /// [`CartridgeError::Misplaced`] for a chip with nowhere to go.
    pub fn new(crt: &Crt) -> Result<Self, CartridgeError> {
        let kind =
            Kind::from_hardware(crt.hardware).ok_or(CartridgeError::Unsupported(crt.hardware))?;
        let mut low_banks: Vec<Option<Vec<u8>>> = vec![None; MAX_BANKS];
        let mut high_banks: Vec<Option<Vec<u8>>> = vec![None; MAX_BANKS];
        let mut banks = 0;
        for chip in &crt.chips {
            let bank = usize::from(chip.bank);
            if bank >= MAX_BANKS {
                return Err(CartridgeError::Misplaced);
            }
            banks = banks.max(bank + 1);
            let (high, offset) = slot(chip.address).ok_or(CartridgeError::Misplaced)?;
            // Ocean's ROMH mirrors ROML, so every chip is a ROML bank.
            let high = high && kind != Kind::Ocean;
            // A chip fits its slot, but for 16 KiB at $8000 filling ROMH too.
            let spans_both = !high && offset == 0 && chip.bytes.len() == 2 * BANK_BYTES;
            if offset + chip.bytes.len() > BANK_BYTES && !spans_both {
                return Err(CartridgeError::Misplaced);
            }
            let mut place = |high: bool, offset: usize, bytes: &[u8]| {
                let banks = if high {
                    &mut high_banks
                } else {
                    &mut low_banks
                };
                let image = banks[bank].get_or_insert_with(|| vec![0xFF; BANK_BYTES]);
                // A chip smaller than its bank repeats through it.
                for (i, byte) in image.iter_mut().enumerate().skip(offset) {
                    *byte = bytes[(i - offset) % bytes.len()];
                }
            };
            let (low_half, high_half) = chip
                .bytes
                .split_at(chip.bytes.len().min(BANK_BYTES - offset));
            place(high, offset, low_half);
            if !high_half.is_empty() {
                place(true, 0, high_half);
            }
        }
        let bank_mask = banks.next_power_of_two().max(1) - 1;
        let mut cartridge = Self {
            kind,
            roml: low_banks,
            romh: high_banks,
            bank_mask,
            bank: 0,
            high_bank: 0,
            exrom: false,
            game: false,
            image_lines: (crt.exrom, crt.game),
            off_at: EPYX_CHARGE_CYCLES,
            locked: false,
        };
        cartridge.reset(0);
        Ok(cartridge)
    }

    /// Which hardware it is.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// RES at cycle `now`: bank 0, the lines as the hardware starts them.
    pub fn reset(&mut self, now: u64) {
        self.bank = 0;
        self.high_bank = 0;
        self.locked = false;
        self.off_at = now + EPYX_CHARGE_CYCLES;
        let (exrom, game) = match self.kind {
            Kind::Generic => self.image_lines,
            Kind::Ocean if self.bank_mask == 63 => (true, false),
            Kind::SuperGames | Kind::Zaxxon | Kind::Ocean => (true, true),
            Kind::SimonsBasic
            | Kind::FunPlay
            | Kind::EpyxFastLoad
            | Kind::System3
            | Kind::Dinamic
            | Kind::MagicDesk => (true, false),
        };
        self.exrom = exrom;
        self.game = game;
    }

    /// The expansion port's lines as the PLA reads them at cycle `now`.
    #[must_use]
    pub const fn lines(&self, now: u64) -> memory::Cartridge {
        let exrom = self.exrom && !(matches!(self.kind, Kind::EpyxFastLoad) && now >= self.off_at);
        memory::Cartridge {
            game: !self.game,
            exrom: !exrom,
        }
    }

    fn roml_byte(&self, bank: usize, addr: u16) -> Option<u8> {
        let image = self.roml.get(bank & self.bank_mask)?.as_ref()?;
        Some(image[usize::from(addr) & (BANK_BYTES - 1)])
    }

    /// The CPU or VIC-II reads ROML at `addr` in cycle `now`; `None` where
    /// no chip answers.
    pub fn read_roml(&mut self, addr: u16, now: u64) -> Option<u8> {
        match self.kind {
            Kind::EpyxFastLoad => self.off_at = now + EPYX_CHARGE_CYCLES,
            Kind::Zaxxon => self.high_bank = usize::from(addr & 0x1000 != 0),
            _ => {}
        }
        self.roml_byte(self.bank, addr)
    }

    /// ROMH at `addr`, read; `None` where no chip answers.
    #[must_use]
    pub fn read_romh(&self, addr: u16) -> Option<u8> {
        if self.kind == Kind::Ocean {
            return self.roml_byte(self.bank, addr);
        }
        let bank = if self.kind == Kind::Zaxxon {
            self.high_bank
        } else {
            self.bank
        };
        let image = self.romh.get(bank & self.bank_mask)?.as_ref()?;
        Some(image[usize::from(addr) & (BANK_BYTES - 1)])
    }

    /// The CPU reads I/O1 (`$DExx`) or I/O2 (`$DFxx`) at `addr` in cycle
    /// `now`; `None` where the cartridge does not answer.
    pub fn read_io(&mut self, addr: u16, now: u64) -> Option<u8> {
        let io1 = addr & 0x0100 == 0;
        let low = usize::from(addr & 0x3F);
        match self.kind {
            Kind::EpyxFastLoad if io1 => {
                self.off_at = now + EPYX_CHARGE_CYCLES;
                None
            }
            // The ROM's last page shows through I/O2 whether it is on or not.
            Kind::EpyxFastLoad => self.roml_byte(0, 0x1F00 | (addr & 0xFF)),
            Kind::SimonsBasic if io1 => {
                self.game = false;
                None
            }
            Kind::System3 | Kind::Dinamic if io1 => {
                self.bank = low;
                None
            }
            _ => None,
        }
    }

    /// The CPU writes `value` to I/O1 or I/O2 at `addr`.
    pub fn write_io(&mut self, addr: u16, value: u8) {
        let io1 = addr & 0x0100 == 0;
        match self.kind {
            // The access switches it, the value written aside.
            Kind::SimonsBasic if io1 => self.game = true,
            Kind::Ocean if io1 => self.bank = usize::from(value & 0x3F),
            Kind::FunPlay if io1 => {
                if value == 0x86 {
                    self.exrom = false;
                } else {
                    self.bank = usize::from(((value >> 3) & 0x07) | ((value & 0x01) << 3));
                }
            }
            Kind::SuperGames if !io1 && !self.locked => {
                self.bank = usize::from(value & 0x03);
                let on = value & 0x04 == 0;
                self.exrom = on;
                self.game = on;
                self.locked = value & 0x08 != 0;
            }
            Kind::System3 if io1 => self.bank = usize::from(addr & 0x3F),
            Kind::MagicDesk if io1 => {
                self.exrom = value & 0x80 == 0;
                self.bank = usize::from(value & 0x7F);
            }
            _ => {}
        }
    }
}

/// Each of a cartridge's banks, a byte saying whether a chip fills it.
fn save_banks(banks: &[Option<Vec<u8>>], w: &mut Writer) {
    for bank in banks {
        w.put(&bank.is_some());
        if let Some(bytes) = bank {
            w.bytes(bytes);
        }
    }
}

fn load_banks(r: &mut Reader<'_>) -> Result<Vec<Option<Vec<u8>>>, SnapshotError> {
    let mut banks = Vec::with_capacity(MAX_BANKS);
    for _ in 0..MAX_BANKS {
        banks.push(if r.get::<bool>()? {
            let bytes = r.bytes_at_most(BANK_BYTES)?;
            Reader::check(bytes.len() == BANK_BYTES)?;
            Some(bytes.to_vec())
        } else {
            None
        });
    }
    Ok(banks)
}

impl snapshot::Save for Cartridge {
    fn save(&self, w: &mut Writer) {
        w.put(&self.kind);
        save_banks(&self.roml, w);
        save_banks(&self.romh, w);
        w.put(&self.bank_mask);
        w.put(&self.bank);
        w.put(&self.high_bank);
        w.put(&self.exrom);
        w.put(&self.game);
        w.put(&self.image_lines);
        w.put(&self.off_at);
        w.put(&self.locked);
    }
}

impl snapshot::Load for Cartridge {
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        let cartridge = Self {
            kind: r.get()?,
            roml: load_banks(r)?,
            romh: load_banks(r)?,
            bank_mask: r.get()?,
            bank: r.get()?,
            high_bank: r.get()?,
            exrom: r.get()?,
            game: r.get()?,
            image_lines: r.get()?,
            off_at: r.get()?,
            locked: r.get()?,
        };
        // Bank registers hold no more than seven bits.
        Reader::check(
            cartridge.bank_mask < MAX_BANKS
                && (cartridge.bank_mask + 1).is_power_of_two()
                && cartridge.bank < 0x80
                && cartridge.high_bank <= 1
                && cartridge.off_at <= r.clock() + EPYX_CHARGE_CYCLES,
        )?;
        Ok(cartridge)
    }
}

#[cfg(test)]
#[path = "cartridge_tests.rs"]
mod tests;
