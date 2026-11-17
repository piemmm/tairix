//! The ROM modules: the session's built-ins in the ROM's order, their images
//! in the host pages, started at reset unless CMOS unplugs them, listed as
//! `OS_Module` 19 and 20 list them, and plugged in and out as `*RMInsert`
//! and `*Unplug` do. A ROM module's chunk number is its place in the
//! inventory, which names its unplug bit.

use alloc::vec::Vec;

use super::header::Field;
use super::image::{self, EntryKind};
use super::inventory::{Rom, INVENTORY};
use super::reasons::NO_MORE_MODULES;
use super::{name, utility, Builtin, ModuleKey, Place, Shape};
use crate::memory::{Memory, MemoryError};
use crate::os::{BootError, Fail, Os, Regs};

/// A ROM module: a built-in and its image.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RomModule {
    /// Its row in the inventory: its chunk number.
    pub(crate) row: usize,
    /// Its Rust.
    pub(crate) builtin: &'static Builtin,
    /// Its image's address.
    pub(crate) image: u32,
    /// Its size word: the image's length and the word's.
    pub(crate) size: u32,
    /// Where its routines' entries begin in its image, a jump of two words
    /// each.
    pub(crate) routines: u32,
}

/// The kernel's own built-in modules.
const KERNEL: [&Builtin; 8] = [
    &utility::UTILITY,
    &crate::resourcefs::RESOURCE_FS,
    &crate::territory::TERRITORY_MANAGER,
    &crate::resources::MESSAGES,
    &crate::msgtrans::MESSAGE_TRANS,
    &crate::territory::uk::UK,
    &crate::international::INTERNATIONAL,
    &crate::intkey::INTERNATIONAL_KEYBOARD,
];

/// The first chunk an unplug bit is kept for.
const FIRST_UNPLUGGABLE: usize = 8;
/// The CMOS byte keeping the unplug bits of each eight chunks from the first
/// unpluggable, in order, as `*Unplug` sets them on RISC OS 5.
const UNPLUG_CMOS: [u8; 15] = [
    0xAD, 0xAE, 0xAF, 0xB0, 0x06, 0x07, 0xD9, 0xDA, 0xDB, 0x12, 0x13, 0x09, 0x82, 0x83, 0x8D,
];
const _: () = assert!(FIRST_UNPLUGGABLE + 8 * UNPLUG_CMOS.len() >= INVENTORY.len());

/// `OS_Module` 19 and 20's section for the main ROM: the only one there is.
const MAIN_ROM: u32 = u32::MAX;

/// The CMOS byte and bit that unplug chunk `row`, if it has them.
fn unplug_bit(row: usize) -> Option<(u8, u8)> {
    let n = row.checked_sub(FIRST_UNPLUGGABLE)?;
    let address = *UNPLUG_CMOS.get(n / 8)?;
    Some((address, 1 << (n % 8)))
}

/// The entries `builtin`'s image has.
fn entries_of(builtin: &Builtin) -> u32 {
    let present = [
        builtin.start.is_some(),
        builtin.init.is_some(),
        builtin.finalise.is_some(),
        builtin.services.is_some(),
        builtin.swis.is_some(),
    ];
    let commands = builtin.commands.iter().map(|command| {
        u32::from(command.run.is_some()) + u32::from(matches!(command.help, super::Help::Code(_)))
    });
    let routines = crate::os::len32(builtin.routines.len());
    present
        .into_iter()
        .map(u32::from)
        .chain(commands)
        .sum::<u32>()
        + routines
}

/// The session's built-ins in the ROM's order, each with its row: the
/// kernel's own and those `more` brings, refused if one is no provided module
/// of the inventory or comes twice.
pub(crate) fn arranged(
    more: &[&'static Builtin],
) -> Result<Vec<(usize, &'static Builtin)>, BootError> {
    let mut out: Vec<(usize, &'static Builtin)> = Vec::new();
    for &builtin in KERNEL.iter().chain(more) {
        let row = INVENTORY
            .iter()
            .position(|rom| rom.title == builtin.title && rom.provided())
            .ok_or(BootError::Module(builtin.title))?;
        if out.iter().any(|&(taken, _)| taken == row) {
            return Err(BootError::Module(builtin.title));
        }
        out.try_reserve(1).map_err(|_| BootError::NoMemory)?;
        out.push((row, builtin));
    }
    out.sort_by_key(|&(row, _)| row);
    Ok(out)
}

/// The host routines the entries of `builtins` take.
pub(crate) fn entry_count(builtins: &[(usize, &'static Builtin)]) -> u32 {
    builtins
        .iter()
        .map(|&(_, builtin)| entries_of(builtin))
        .sum()
}

/// The ROM chain as installed in the host pages.
pub(crate) struct Installed {
    /// The ROM modules.
    pub(crate) roms: Vec<RomModule>,
    /// What each built-in entry's routine enters.
    pub(crate) entries: Vec<(usize, EntryKind)>,
    /// The word after the chain, which holds the empty environment string.
    pub(crate) empty: u32,
}

/// Installs the ROM chain of `builtins` in the host pages, entry `n`'s
/// routine at `routine(n)`.
pub(crate) fn install(
    memory: &mut Memory,
    builtins: &[(usize, &'static Builtin)],
    routine: impl Fn(u32) -> u32,
) -> Result<Installed, MemoryError> {
    let mut listed: Vec<(&Rom, &'static Builtin)> = Vec::new();
    listed.try_reserve(builtins.len())?;
    listed.extend(
        builtins
            .iter()
            .filter_map(|&(row, b)| Some((INVENTORY.get(row)?, b))),
    );
    let mut entries = Vec::new();
    let chain = image::chain(&listed, &mut entries, routine)?;
    let base = memory.install_host(&chain.words)?;
    let mut roms = Vec::new();
    roms.try_reserve(builtins.len())?;
    for (&(row, builtin), &(offset, size, routines)) in builtins.iter().zip(&chain.images) {
        roms.push(RomModule {
            row,
            builtin,
            image: base + offset,
            size,
            routines,
        });
    }
    Ok(Installed {
        roms,
        entries,
        empty: base + chain.empty,
    })
}

impl Os<'_> {
    /// Starts the ROM modules at reset, in order, but those CMOS unplugs,
    /// then issues `Service_PostInit`.
    pub(crate) fn start_roms(&mut self) -> Result<(), BootError> {
        for place in 0..self.world.modules.roms.len() {
            let Some(rom) = self.world.modules.roms.get(place).copied() else {
                break;
            };
            let unplugged = self.unplugged(rom.row).map_err(|_| BootError::NoMemory)?;
            if !unplugged {
                self.start_rom(place)
                    .map_err(|_| BootError::Module(rom.builtin.title))?;
            }
        }
        let mut regs = Regs::default();
        regs.r[1] = POST_INIT;
        self.service(&mut regs).map_err(|_| BootError::NoMemory)?;
        Ok(())
    }

    /// Starts the session's `place`th ROM module in its own image, with an
    /// empty environment, last in the chain.
    fn start_rom(&mut self, place: usize) -> Result<ModuleKey, Fail> {
        let Some(rom) = self.world.modules.roms.get(place).copied() else {
            return Err(self.no_memory());
        };
        let (header, bits) = self.checked_header(rom.image, rom.size)?;
        let shape = Shape {
            code: rom.image,
            header,
            bits,
            rom: Some(place),
            hardware: 0,
        };
        let empty = self.world.modules.empty;
        self.link_and_init(shape, Place::End, empty)
    }

    /// Starts the ROM module `title` names afresh, any namesake killed:
    /// plugged back in first when `plug`. None if no ROM module has the
    /// title.
    pub(crate) fn restart_rom(
        &mut self,
        title: &[u8],
        plug: bool,
    ) -> Result<Option<ModuleKey>, Fail> {
        let Some(place) = self.rom_named(title) else {
            return Ok(None);
        };
        if plug {
            self.set_unplugged(place, false)?;
        }
        if let Some(active) = self.find_module(title)? {
            self.kill_and_free(active)?;
        }
        self.start_rom(place).map(Some)
    }

    /// The session's ROM module `title` names.
    pub(crate) fn rom_named(&self, title: &[u8]) -> Option<usize> {
        self.world
            .modules
            .roms
            .iter()
            .position(|rom| name::names(title, rom.builtin.title.as_bytes()))
    }

    /// Whether CMOS unplugs chunk `row`.
    pub(crate) fn unplugged(&mut self, row: usize) -> Result<bool, Fail> {
        let Some((address, bit)) = unplug_bit(row) else {
            return Ok(false);
        };
        Ok(self.read_cmos(address)? & bit != 0)
    }

    /// Sets or clears the unplug bit of the session's `place`th ROM module,
    /// CMOS written only if the bit changes; whether it has one.
    pub(crate) fn set_unplugged(&mut self, place: usize, unplug: bool) -> Result<bool, Fail> {
        let Some((address, bit)) = self
            .world
            .modules
            .roms
            .get(place)
            .and_then(|rom| unplug_bit(rom.row))
        else {
            return Ok(false);
        };
        let old = self.read_cmos(address)?;
        let new = if unplug { old | bit } else { old & !bit };
        if new != old {
            self.write_cmos(address, new)?;
        }
        Ok(true)
    }

    /// The status `OS_Module` 19 gives the session's `place`th ROM module:
    /// -1 unplugged, 0 dormant, 1 active, 2 running — active and holding the
    /// current active object.
    pub(crate) fn rom_status(&mut self, place: usize) -> Result<u32, Fail> {
        let Some(rom) = self.world.modules.roms.get(place).copied() else {
            return Ok(0);
        };
        if self.unplugged(rom.row)? {
            return Ok(u32::MAX);
        }
        let active = self.world.env.active_object();
        let module = self
            .world
            .modules
            .chain()
            .iter()
            .filter_map(|&key| self.world.modules.get(key))
            .find(|module| module.shape.rom == Some(place));
        Ok(match module {
            None => 0,
            Some(module) if active.wrapping_sub(module.shape.code) < module.shape.header.size() => {
                2
            }
            Some(_) => 1,
        })
    }

    /// `OS_Module` 19 and 20: the ROM module R1 places into R2's section,
    /// its version in R6 too for 20. The main ROM is the only section.
    pub(crate) fn rom_modules(&mut self, regs: &mut Regs, with_version: bool) -> Result<(), Fail> {
        let place = regs.r[1] as usize;
        let rom = (regs.r[2] == MAIN_ROM)
            .then(|| self.world.modules.roms.get(place).copied())
            .flatten();
        let Some(rom) = rom else {
            return Err(self.error(NO_MORE_MODULES));
        };
        let status = self.rom_status(place)?;
        let title = self.read_word(rom.image.wrapping_add(Field::Title.offset()))?;
        regs.r[1] = crate::os::len32(place + 1);
        regs.r[2] = MAIN_ROM;
        regs.r[3] = rom.image.wrapping_add(title);
        regs.r[4] = status;
        if with_version {
            regs.r[6] = INVENTORY.get(rom.row).map_or(0, Rom::bcd);
        }
        Ok(())
    }
}

/// `Service_PostInit`, once every ROM module has started.
const POST_INIT: u32 = 0x73;
