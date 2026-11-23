//! The session's CMOS RAM (`plans/RISCOS-EMULATOR.md` §4.5): the 256 bytes
//! `OS_Byte` 161 and 162 and `OS_NVMemory` reach by logical address, held by
//! the kernel alone, starting as a CMOS reset leaves them, the checksum kept
//! as each write changes them.

use crate::error::Error;
use crate::os::{Fail, Os, Regs};

/// The CMOS RAM's size.
pub const SIZE: u32 = 256;
/// Where the checksum is, by logical address.
const CHECKSUM: u8 = 0xEF;
/// The Econet station number's location, which nothing here changes.
const STATION: u8 = 0x00;
/// The checksum's seed.
const SEED: u8 = 1;
/// The configured filing system: the emulator's discs are HostFS.
pub const FILE_LANG: u8 = 0x05;
/// FileSwitch's options: names truncated rather than refused in bit 0.
pub const FILESWITCH_OPTIONS: u8 = 0x1C;
/// HostFS's filing system number: RPCEmu's HostFS's (recorded under RISC OS
/// 5), which a program that looks for HostFS asks after.
pub const HOSTFS: u8 = 153;
/// The configured country.
pub(crate) const COUNTRY: u8 = 0xBA;
/// The configured territory, its low bit inverted as it is kept.
pub(crate) const TERRITORY: u8 = 0x18;
/// The time zone, in quarter hours of standard time.
pub(crate) const TIME_ZONE: u8 = 0x8B;
/// Daylight saving in force, in bit 7, among the alarm's settings.
pub(crate) const ALARM_AND_TIME: u8 = 0xDC;
pub(crate) const DST: u8 = 1 << 7;
/// The key auto-repeat delay and rate.
pub(crate) const KEY_DELAY: u8 = 0x0C;
pub(crate) const KEY_REPEAT: u8 = 0x0D;
/// The printer ignore character.
pub(crate) const PRINTER_IGNORE: u8 = 0x0E;
/// Printer and serial settings.
pub(crate) const PRINT_SERIAL: u8 = 0x0F;
/// Data bits, boot and the bell's volume.
pub(crate) const DBTB: u8 = 0x10;
/// The keyboard's start-up state.
pub(crate) const START: u8 = 0x0B;
/// Printing and sound options, `DumpFormat` in the low five bits.
pub(crate) const PRINT_SOUND: u8 = 0x84;
/// The TV position and interlace.
pub(crate) const MODE_TV: u8 = 0x0A;
/// The printer buffer's pages.
pub(crate) const PRINTER_BUFFER: u8 = 0x19;
/// The pointing device.
pub(crate) const MOUSE_TYPE: u8 = 0x1D;
/// The monitor type and sync.
pub(crate) const VDU: u8 = 0x85;
/// The font cache, in 4 KiB units.
pub(crate) const FONT_SIZE: u8 = 0x86;
/// The pages of the screen, the RAM disc, the system heap and the RMA
/// beyond what they start with, and the system sprite area.
pub(crate) const SCREEN_SIZE: u8 = 0x8F;
pub(crate) const RAM_DISC: u8 = 0x90;
pub(crate) const SYSTEM_HEAP: u8 = 0x91;
pub(crate) const RMA_SIZE: u8 = 0x92;
/// The pages of the system sprite area.
pub const SPRITE_SIZE: u8 = 0x93;
/// The low bits of a size location — the screen's, the RAM disc's, the
/// RMA's, the system sprite area's, the printer buffer's — that count its
/// pages.
pub const PAGES_BITS: u8 = 7;
/// [`PAGES_BITS`] as a mask.
const PAGES_MASK: u8 = (1 << PAGES_BITS) - 1;
/// The configured language.
pub(crate) const LANGUAGE: u8 = 0xB9;
/// The current year's century.
const YEAR_HIGH: u8 = 0x81;
/// The sound system's default: its speaker, volume and channel.
const SOUND_DEFAULT: u8 = 0x94;
/// The mouse's multiplier, a signed byte.
pub(crate) const MOUSE_STEP: u8 = 0xC2;
/// The cache's and the mode's automatic bits, among others.
pub(crate) const SYSTEM_SPEED: u8 = 0xC3;
/// The configured mode.
pub(crate) const WIMP_MODE: u8 = 0xC4;

/// `OS_Byte` 161 and 162: read and write a CMOS location.
const READ_CMOS: u32 = 161;
const WRITE_CMOS: u32 = 162;

/// The values a CMOS reset writes, by logical address, for the settings the
/// kernel's own keywords and start-up read: genuine RISC OS 5's, but for the
/// configured filing system, HostFS, the emulator's discs; the country; and
/// the display's monitor type and sync. Every other location, the settings
/// of modules the emulator does not provide among them, resets to zero.
const DEFAULTS: [(u8, u8); 17] = [
    (KEY_DELAY, 32),
    (KEY_REPEAT, 8),
    (FILE_LANG, HOSTFS),
    (FONT_SIZE, 64),
    (PRINTER_IGNORE, 10),
    (SYSTEM_HEAP, 8),
    (MODE_TV, 0x10), // *TV 0,1
    (PRINT_SERIAL, (3 << 2) | (1 << 5)),
    (DBTB, (1 << 4) | (4 << 5)),
    (START, 4 | (2 << 3) | (1 << 6)),
    (SOUND_DEFAULT, 0xF0), // speaker on, volume 7, channel 1
    (COUNTRY, 1),          // the UK, the one territory the emulator has
    (YEAR_HIGH, 20),
    (MOUSE_STEP, 2),
    (SYSTEM_SPEED, (1 << 2) | (1 << 4) | (1 << 6)),
    (PRINT_SOUND, 0b1010_0100),
    (VDU, 0x7C), // monitor type and sync automatic: the display is a window
];

/// A setting spread over bit fields of CMOS: the value that means it is
/// automatic, the most any other may be, and each field's location, its
/// mask from bit 0, and where its bits are in the value and in the byte.
pub(crate) struct Spread {
    pub(crate) automatic: u32,
    pub(crate) most: u32,
    fields: &'static [(u8, u8, u8, u8)],
}

/// The configured mode: automatic in a bit of its own.
pub(crate) const MODE: Spread = Spread {
    automatic: 256,
    most: 255,
    fields: &[(WIMP_MODE, 0xFF, 0, 0), (SYSTEM_SPEED, 1, 8, 4)],
};
/// The configured monitor type: one the PRM numbers, 0 to 5.
pub(crate) const MONITOR_TYPE: Spread = Spread {
    automatic: 31,
    most: 5,
    fields: &[(VDU, 0x1F, 0, 2)],
};
/// The configured sync.
pub(crate) const SYNC: Spread = Spread {
    automatic: 3,
    most: 1,
    fields: &[(VDU, 1, 0, 0), (VDU, 1, 1, 7)],
};

/// "Nothing readable at this address".
const NOT_READABLE: Error = Error {
    number: 0x410,
    token: "MemoryNotReadable",
};
/// "No writable memory at this address".
const NOT_WRITABLE: Error = Error {
    number: 0x411,
    token: "MemoryNotWritable",
};

/// What a CMOS reset takes from the TAIRiX locale: its standard time's
/// offset from UTC in quarter hours, and whether daylight saving is in force.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Locale {
    pub(crate) time_zone: i8,
    pub(crate) dst: bool,
}

/// The CMOS RAM, by logical address, and the locale a reset sets it for.
pub(crate) struct Cmos {
    bytes: [u8; SIZE as usize],
    locale: Locale,
}

impl Cmos {
    /// As a CMOS reset leaves it for `locale`.
    pub(crate) fn new(locale: Locale) -> Self {
        let mut cmos = Self {
            bytes: [0; SIZE as usize],
            locale,
        };
        for logical in 0..CHECKSUM {
            if let Some(value) = cmos.reset_value(u32::from(logical)) {
                cmos.bytes[usize::from(logical)] = value;
            }
        }
        cmos.bytes[usize::from(CHECKSUM)] = cmos.checksum();
        cmos
    }

    /// The checksum of every location below it.
    fn checksum(&self) -> u8 {
        self.bytes[..usize::from(CHECKSUM)]
            .iter()
            .fold(SEED, |sum, &byte| sum.wrapping_add(byte))
    }

    /// Logical location `logical`'s value.
    pub(crate) fn read(&self, logical: u8) -> u8 {
        self.bytes[usize::from(logical)]
    }

    /// Writes `value` to logical location `logical`, as `OS_Byte` 162 does:
    /// every location but the station number, the checksum moved by the
    /// change, so it stays as right or as wrong as it was. Whether it was
    /// written.
    pub(crate) fn write(&mut self, logical: u8, value: u8) -> bool {
        if logical == STATION {
            return false;
        }
        let old = core::mem::replace(&mut self.bytes[usize::from(logical)], value);
        if logical < CHECKSUM {
            let sum = &mut self.bytes[usize::from(CHECKSUM)];
            *sum = sum.wrapping_add(value).wrapping_sub(old);
        }
        true
    }

    /// What a CMOS reset writes to `logical`; none for the station number
    /// and the checksum, which it leaves.
    fn reset_value(&self, logical: u32) -> Option<u8> {
        let logical = u8::try_from(logical).ok()?;
        if logical == STATION || logical >= CHECKSUM {
            return None;
        }
        let default = DEFAULTS
            .iter()
            .find(|&&(at, _)| at == logical)
            .map_or(0, |&(_, value)| value);
        Some(match logical {
            TIME_ZONE => self.locale.time_zone.cast_unsigned(),
            ALARM_AND_TIME if self.locale.dst => default | DST,
            _ => default,
        })
    }
}

impl Os<'_> {
    /// CMOS location `logical`, read through `OS_Byte` 161 as RISC OS's own
    /// readers read it.
    pub fn read_cmos(&mut self, logical: u8) -> Result<u8, Fail> {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[READ_CMOS, u32::from(logical)]);
        self.perform(crate::swi::X | crate::swi::os::BYTE, &mut regs)?;
        Ok(regs.byte(2))
    }

    /// The pages size location `logical` counts: its low [`PAGES_BITS`].
    pub fn cmos_pages(&mut self, logical: u8) -> Result<u8, Fail> {
        Ok(self.read_cmos(logical)? & PAGES_MASK)
    }

    /// `value` written to CMOS location `logical` through `OS_Byte` 162.
    pub fn write_cmos(&mut self, logical: u8, value: u8) -> Result<(), Fail> {
        let mut regs = Regs::default();
        regs.r[..3].copy_from_slice(&[WRITE_CMOS, u32::from(logical), u32::from(value)]);
        self.perform(crate::swi::X | crate::swi::os::BYTE, &mut regs)
    }

    /// `spread`'s value, read through `OS_Byte`; -1 where it is automatic,
    /// or past the most it may be.
    pub(crate) fn read_spread(&mut self, spread: &Spread) -> Result<u32, Fail> {
        let mut value = 0;
        for &(location, mask, at_value, at_byte) in spread.fields {
            let bits = (self.read_cmos(location)? >> at_byte) & mask;
            value |= u32::from(bits) << at_value;
        }
        Ok(if value > spread.most { u32::MAX } else { value })
    }

    /// `value` written into `spread`'s fields through `OS_Byte`.
    pub(crate) fn write_spread(&mut self, spread: &Spread, value: u32) -> Result<(), Fail> {
        for &(location, mask, at_value, at_byte) in spread.fields {
            let old = self.read_cmos(location)?;
            let bits = (value >> at_value).to_le_bytes()[0] & mask;
            self.write_cmos(location, (old & !(mask << at_byte)) | (bits << at_byte))?;
        }
        Ok(())
    }

    /// `OS_Byte` 161: R2 the location R1 names, zero past the end.
    pub(crate) fn byte_read_cmos(&mut self, regs: &mut Regs) {
        regs.r[2] =
            u8::try_from(regs.r[1]).map_or(0, |logical| u32::from(self.world.cmos.read(logical)));
    }

    /// `OS_Byte` 162: R2's low byte to the location R1 names.
    pub(crate) fn byte_write_cmos(&mut self, regs: &mut Regs) {
        if let Ok(logical) = u8::try_from(regs.r[1]) {
            self.world.cmos.write(logical, regs.byte(2));
        }
    }

    /// `OS_NVMemory`.
    pub(crate) fn nv_memory(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        match regs.r[0] {
            0 => regs.r[1] = SIZE,
            1 => {
                let logical = u8::try_from(regs.r[1]).map_err(|_| self.error(NOT_READABLE))?;
                regs.r[2] = u32::from(self.world.cmos.read(logical));
            }
            2 => {
                let logical = u8::try_from(regs.r[1]).map_err(|_| self.error(NOT_WRITABLE))?;
                self.world.cmos.write(logical, regs.byte(2));
            }
            3 => {
                let (start, buffer, length) = (regs.r[1], regs.r[2], regs.r[3]);
                for i in 0..length {
                    let logical = u8::try_from(start.wrapping_add(i))
                        .map_err(|_| self.error(NOT_READABLE))?;
                    let value = self.world.cmos.read(logical);
                    self.write_byte(buffer.wrapping_add(i), value)?;
                }
            }
            4 => {
                let (start, buffer, length) = (regs.r[1], regs.r[2], regs.r[3]);
                for i in 0..length {
                    let logical = u8::try_from(start.wrapping_add(i))
                        .map_err(|_| self.error(NOT_WRITABLE))?;
                    let value = self.read_byte(buffer.wrapping_add(i))?;
                    self.world.cmos.write(logical, value);
                }
            }
            6 => {
                regs.r[2] = self
                    .world
                    .cmos
                    .reset_value(regs.r[1])
                    .map_or(u32::MAX, u32::from);
            }
            // Setting the station number takes a passphrase, and none is
            // known here.
            7 => return Err(self.error(NOT_WRITABLE)),
            _ => return Err(self.error(crate::kheap::UNKNOWN_REASON)),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
