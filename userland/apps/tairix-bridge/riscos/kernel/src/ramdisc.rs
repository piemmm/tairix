//! The RAM disc's dynamic area (`plans/RISCOS-EMULATOR.md` §6): area 5, a
//! pool of pages the kernel makes at reset as large as `*Configure RAMFsSize`
//! says, and its handler, which resizes the pool only while the disc on it
//! holds nothing, the filing system that keeps the disc started afresh after.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_riscos_arm::mmu::PAGE_SIZE;
use tairix_riscos_arm::psr;
use tairix_riscos_arm::reg::Reg;

use crate::dynamic::UNKNOWN_AREA_HANDLER;
use crate::error::Error;
use crate::host::Routine;
use crate::memory::handler_reason as reason;
use crate::memory::{AreaFlags, Bits, Create, LogicalOp, PageQuery, PhysicalOp};
use crate::os::{Fail, Os, Unwind};

/// The area's number.
pub const AREA: u32 = 5;

/// Its title.
const TITLE: &[u8] = b"RAM disc";

/// Its pages' access privilege: privileged code's alone.
pub const ACCESS: u32 = 2;

/// Its flags, as RISC OS 5 gives them: a pool of pages it names, which only
/// privileged code may reach.
const FLAGS: u32 = AreaFlags::PMP | AreaFlags::NEEDS_SPECIFIC_PAGES | ACCESS;

/// The most address space its window takes, however much RAM the session
/// has: half of what a 32-bit address reaches.
const WINDOW_MOST: u64 = 1 << 31;

/// The filing system whose disc the area holds.
const FILING_SYSTEM: &[u8] = b"RamFS";

/// A resize refused while the disc holds anything, as RISC OS 5 refuses it.
const IN_USE: Error = Error {
    number: 0x1C3,
    token: "RAMDiscInUse",
};

/// What the area asks of the filing system on it before it resizes, held by
/// that filing system while it is active.
pub struct Occupant {
    /// Whether the disc holds anything a resize would lose.
    pub occupied: fn(&mut Os<'_>) -> bool,
}

impl Os<'_> {
    /// Makes the area at reset, its pool as many pages as CMOS gives, or as
    /// many of them as the session has: a short pool is a smaller disc.
    pub(crate) fn create_ram_disc(&mut self) -> Result<(), Fail> {
        let pages = i32::from(self.cmos_pages(crate::cmos::RAM_DISC)?);
        let window = u32::try_from(self.ceiling_bytes().min(WINDOW_MOST)).unwrap_or(u32::MAX)
            & !(PAGE_SIZE - 1);
        let request = Create {
            number: Some(AREA),
            size: 0,
            base: None,
            flags: FLAGS,
            max: Some(window),
            handler: Some(self.world.routine_address(Routine::RamDiscArea)),
            workspace: None,
            title: Some(TITLE),
            pool: 0,
            caller: Bits::Bit32,
        };
        if let Err(error) = self.memory.create_kernel_area(&request, self.world) {
            return Err(self.fail_area(error));
        }
        match self.resize_ram_disc_pool(pages) {
            Ok(()) | Err(Fail::Error(_)) => Ok(()),
            Err(unwind) => Err(unwind),
        }
    }

    /// The area's handler, entered as its routine is: `ResizePMP`, all a
    /// pool's handler is asked.
    pub(crate) fn ram_disc_area_entered(&mut self) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let asked = cpu.reg(Reg::field(0));
        let pages = cpu.reg(Reg::field(1)).cast_signed();
        let outcome = if asked == reason::RESIZE_PMP {
            self.resize_ram_disc(pages)
        } else {
            Err(self.error(UNKNOWN_AREA_HANDLER))
        };
        let refusal = match outcome {
            Ok(()) => None,
            Err(Fail::Error(block)) => Some(block),
            Err(Fail::Unwind(unwind)) => return Err(unwind),
        };
        let cpu = self.world.processor.cpu();
        let flags = match refusal {
            Some(block) => {
                cpu.set_reg(Reg::field(0), block);
                cpu.cpsr() | psr::V
            }
            None => cpu.cpsr() & !psr::V,
        };
        cpu.set_nzcv(flags);
        self.return_to_lr();
        Ok(())
    }

    /// The pool `pages` larger, or smaller where negative: refused while the
    /// disc holds anything; otherwise the filing system let go of the disc
    /// first and started afresh on the new one after, however the resize
    /// went.
    fn resize_ram_disc(&mut self, pages: i32) -> Result<(), Fail> {
        if pages == 0 {
            return Ok(());
        }
        let occupied = self
            .extension::<Occupant>()
            .map(|occupant| occupant.occupied);
        if occupied.is_some_and(|occupied| occupied(self)) {
            return Err(self.error(IN_USE));
        }
        if let Some(active) = self.find_module(FILING_SYSTEM)? {
            self.kill_and_free(active)?;
        }
        let resized = self.resize_ram_disc_pool(pages);
        let plugged = match self.rom_named(FILING_SYSTEM) {
            Some(place) => {
                let row = self.world.modules.roms.get(place).map(|rom| rom.row);
                match row {
                    Some(row) => !self.unplugged(row)?,
                    None => false,
                }
            }
            None => false,
        };
        if plugged {
            // An empty area is no disc: the filing system refuses to start on
            // one, and stays dormant until the area grows.
            match self.restart_rom(FILING_SYSTEM, false) {
                Ok(_) | Err(Fail::Error(_)) => {}
                Err(unwind) => return Err(unwind),
            }
        }
        resized
    }

    /// The pool `pages` larger or smaller, every slot of its page list
    /// holding a page: what could not be claimed is trimmed from the list.
    fn resize_ram_disc_pool(&mut self, pages: i32) -> Result<(), Fail> {
        let held = match self.memory.area_info(AREA) {
            Ok(info) => info.pool.map_or(0, |(pages, _)| pages),
            Err(error) => return Err(self.fail_area(error)),
        };
        let (changed, page) = if pages >= 0 {
            match self.memory.pool_resize(AREA, pages) {
                Ok(added) => (held..held + added, -2),
                Err(error) => return Err(self.fail_area(error)),
            }
        } else {
            let released = held - pages.unsigned_abs().min(held)..held;
            self.unmap_ram_disc_slots(released.clone())?;
            (released, -1)
        };
        let mut slots: Vec<PhysicalOp> = Vec::new();
        if slots.try_reserve_exact(changed.len()).is_err() {
            return Err(self.no_memory());
        }
        slots.extend(changed.map(|slot| PhysicalOp {
            slot,
            page,
            flags: 0,
        }));
        let moved = self.memory.pool_physical(AREA, &slots, self.world);
        let kept = match self.memory.area_info(AREA) {
            Ok(info) => info.pool.unwrap_or((0, 0)),
            Err(error) => return Err(self.fail_area(error)),
        };
        let spare = kept.1.saturating_sub(kept.0);
        if spare > 0 {
            let trim = i32::try_from(spare).map_or(i32::MIN, |spare| -spare);
            if let Err(error) = self.memory.pool_resize(AREA, trim) {
                return Err(self.fail_area(error));
            }
        }
        moved.map_err(|(error, _)| self.fail_area(error))
    }

    /// The window's pages showing `slots` unmapped: a page may not leave the
    /// pool while it is mapped, and the filing system that mapped it has gone.
    fn unmap_ram_disc_slots(&mut self, slots: Range<u32>) -> Result<(), Fail> {
        let mut queries: Vec<PageQuery> = Vec::new();
        if queries.try_reserve_exact(slots.len()).is_err() {
            return Err(self.no_memory());
        }
        queries.extend(slots.map(|slot| PageQuery {
            slot: Some(slot),
            ..PageQuery::default()
        }));
        if let Err((error, _)) = self.memory.pool_query(AREA, &mut queries) {
            return Err(self.fail_area(error));
        }
        let mut unmapped: Vec<LogicalOp> = Vec::new();
        if unmapped.try_reserve_exact(queries.len()).is_err() {
            return Err(self.no_memory());
        }
        unmapped.extend(
            queries
                .iter()
                .filter_map(|query| query.window)
                .map(|window| LogicalOp {
                    window,
                    slot: None,
                    flags: 0,
                }),
        );
        self.memory
            .pool_logical(AREA, &unmapped)
            .map_err(|(error, _)| self.fail_area(error))
    }
}

#[cfg(test)]
mod tests;
