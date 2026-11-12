//! The expanded memory board: up to 512 pages of 16 KiB, and four page
//! registers that show any of them in the four 16 KiB windows of a 64 KiB
//! frame, as LIM EMS 4.0 asks of a board. The board holds one page more,
//! which its manager keeps its tables in and no register can show.
//!
//! Each register is two ports, low byte first: the page number, and in the
//! high byte the page's ninth bit and, in bit 7, whether the window shows
//! it. Both read back.

/// A page's size, which LIM fixes.
pub const PAGE: u32 = 0x4000;
/// The windows a frame holds.
pub const WINDOWS: usize = 4;
/// The most pages a board holds: 8 MiB, the 8-bit era's ceiling.
pub const MAX_PAGES: u16 = 512;
/// The ports the registers take.
pub const PORTS: u16 = 8;
/// Where the registers may be jumpered.
pub const BASES: [u16; 7] = [0x208, 0x218, 0x258, 0x268, 0x2A8, 0x2B8, 0x2E8];
/// Where the frame may be.
pub const FRAMES: [u32; 2] = [0xD_0000, 0xE_0000];
/// A register's high byte: the window shows its page.
const SHOWN: u8 = 0x80;
/// A register's high byte: the page number's ninth bit.
const PAGE_HIGH: u8 = 0x01;

/// The register value that shows `page`, or empties the window.
#[must_use]
pub fn register(page: Option<u16>) -> [u8; 2] {
    match page {
        Some(page) => {
            let [low, high] = page.to_le_bytes();
            [low, (high & PAGE_HIGH) | SHOWN]
        }
        None => [0, 0],
    }
}

/// Whether a register can hold `register`: its high byte has no other bits.
#[must_use]
pub const fn fits(register: [u8; 2]) -> bool {
    register[1] & !(SHOWN | PAGE_HIGH) == 0
}

/// The page register value `register` shows on a board of `pages`, if any.
#[must_use]
pub fn page_of(register: [u8; 2], pages: u16) -> Option<u16> {
    let [low, high] = register;
    let page = u16::from_le_bytes([low, high & PAGE_HIGH]);
    (high & SHOWN != 0 && page < pages).then_some(page)
}

/// The board's registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ems {
    /// The pages the registers can show.
    pages: u16,
    registers: [[u8; 2]; WINDOWS],
}

impl Ems {
    /// A board of `pages` pages, every window empty.
    #[must_use]
    pub const fn new(pages: u16) -> Self {
        Self {
            pages,
            registers: [[0; 2]; WINDOWS],
        }
    }

    /// The pages the registers can show.
    #[must_use]
    pub const fn pages(&self) -> u16 {
        self.pages
    }

    /// The page window `n` shows, if any: a page the board holds, behind an
    /// enabled register.
    #[must_use]
    pub fn shown(&self, n: usize) -> Option<u16> {
        page_of(*self.registers.get(n)?, self.pages)
    }

    /// Reads register byte `offset`.
    #[must_use]
    pub fn input(&self, offset: u16) -> u8 {
        let n = usize::from(offset / 2) % WINDOWS;
        self.registers[n][usize::from(offset & 1)]
    }

    /// Writes register byte `offset`; returns the window it may have
    /// changed.
    pub fn output(&mut self, offset: u16, value: u8) -> usize {
        let n = usize::from(offset / 2) % WINDOWS;
        let mask = if offset & 1 == 0 {
            0xFF
        } else {
            SHOWN | PAGE_HIGH
        };
        self.registers[n][usize::from(offset & 1)] = value & mask;
        n
    }
}

crate::snapshot::state!(Ems { registers } as built { pages });

#[cfg(test)]
#[path = "ems_tests.rs"]
mod tests;
