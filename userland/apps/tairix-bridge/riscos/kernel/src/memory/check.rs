//! `OS_Memory` 24's view of a range: the accesses all of it allows, and
//! those some of it does, as the page tables say.

use tairix_riscos_arm::mmu::{Rights, PAGE_SIZE};

use super::layout::APPLICATION;
use super::Memory;

/// User mode may read all of it.
pub const USER_READ: u32 = 1 << 0;
/// User mode may write all of it.
pub const USER_WRITE: u32 = 1 << 1;
/// The privileged modes may read all of it.
pub const PRIVILEGED_READ: u32 = 1 << 2;
/// The privileged modes may write all of it.
pub const PRIVILEGED_WRITE: u32 = 1 << 3;
/// The four, for some of it: each "all" bit moved up four.
const PARTIAL_SHIFT: u32 = 4;
/// All of it lies in abortable areas.
pub const ABORTABLE: u32 = 1 << 9;
/// User mode may run none of it.
pub const USER_NO_EXECUTE: u32 = 1 << 10;
/// The privileged modes may run none of it.
pub const PRIVILEGED_NO_EXECUTE: u32 = 1 << 11;
/// Some of it lies in abortable areas.
const PARTIAL_ABORTABLE: u32 = 1 << 13;
/// The "no execute" bits, for some of it.
const PARTIAL_NO_EXECUTE: u32 = 0b11 << 14;

/// The "all of it" bits a report starts with and each page may clear.
const COMPLETE: u32 = USER_READ
    | USER_WRITE
    | PRIVILEGED_READ
    | PRIVILEGED_WRITE
    | ABORTABLE
    | USER_NO_EXECUTE
    | PRIVILEGED_NO_EXECUTE;

impl Memory {
    /// Where the run of pages mapped alike from `address` ends: the current
    /// slot's top, a plain area's mapped end, or the next page.
    fn uniform_run(&self, address: u32) -> u64 {
        let next_page = (u64::from(address) & !u64::from(PAGE_SIZE - 1)) + u64::from(PAGE_SIZE);
        if let Some(size) = self.current_slot().and_then(|slot| self.slot_size(slot)) {
            let top = APPLICATION + size;
            if (APPLICATION..top).contains(&address) {
                return u64::from(top);
            }
        }
        self.area_run(address).map_or(next_page, u64::from)
    }

    /// `OS_Memory` 24's flags for `start` to `end` (exclusive; one byte if
    /// they are equal).
    #[must_use]
    pub fn check_access(&mut self, start: u32, end: u64) -> u32 {
        let end = end.max(u64::from(start) + 1);
        let mut complete = COMPLETE;
        let mut partial = 0;
        let mut at = u64::from(start);
        while at < end {
            let Ok(address) = u32::try_from(at) else {
                complete = 0;
                break;
            };
            let run_end = self.uniform_run(address).min(end);
            let flags = self.page_flags(address);
            complete &= flags;
            partial |= ((flags & 0xF) << PARTIAL_SHIFT)
                | if flags & ABORTABLE != 0 {
                    PARTIAL_ABORTABLE
                } else {
                    0
                }
                | ((flags & (USER_NO_EXECUTE | PRIVILEGED_NO_EXECUTE)) << 4 & PARTIAL_NO_EXECUTE);
            at = run_end;
        }
        complete | partial
    }

    /// Whether every page from `start` to `end` (exclusive; one byte if they
    /// are equal) gives all of `rights`, flags as [`Memory::check_access`]
    /// gives them: the walk ends at the first that does not.
    #[must_use]
    pub fn grants(&mut self, start: u32, end: u64, rights: u32) -> bool {
        let end = end.max(u64::from(start) + 1);
        let mut at = u64::from(start);
        while at < end {
            let Ok(address) = u32::try_from(at) else {
                return false;
            };
            if self.page_flags(address) & rights != rights {
                return false;
            }
            at = self.uniform_run(address).min(end);
        }
        true
    }

    /// Whether the page tables refuse an access at `address`: a write if
    /// `write`, from a privileged mode if `privileged`.
    #[must_use]
    pub fn refuses(&self, address: u32, write: bool, privileged: bool) -> bool {
        let mmu = &self.mmu;
        let Some((_, access)) = mmu.lookup(mmu.table_of(address), address) else {
            return true;
        };
        match if privileged {
            access.privileged
        } else {
            access.user
        } {
            Rights::None => true,
            Rights::Read => write,
            Rights::ReadWrite => false,
        }
    }

    /// The flags one page's mapping gives.
    fn page_flags(&mut self, address: u32) -> u32 {
        let abortable = if self.abortable_at(address).is_some() {
            ABORTABLE
        } else {
            0
        };
        let mmu = &self.mmu;
        let Some((_, access)) = mmu.lookup(mmu.table_of(address), address) else {
            return abortable;
        };
        let rights = |rights: Rights, read: u32, write: u32| match rights {
            Rights::None => 0,
            Rights::Read => read,
            Rights::ReadWrite => read | write,
        };
        let no_execute = if access.execute {
            0
        } else {
            USER_NO_EXECUTE | PRIVILEGED_NO_EXECUTE
        };
        rights(access.user, USER_READ, USER_WRITE)
            | rights(access.privileged, PRIVILEGED_READ, PRIVILEGED_WRITE)
            | abortable
            | no_execute
    }
}
