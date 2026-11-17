//! The physical page map as the memory SWIs read it: each page's place and
//! flags, which page an address shows, runs of free pages, and the access
//! privileges by number.

use tairix_riscos_arm::mmu::{Access, Frame, Rights, PAGE_SIZE};

use super::layout::APPLICATION;
use super::{access_privilege, Holder, Memory};

/// The page flag marking a page claimed exclusively.
const UNAVAILABLE: u32 = 1 << 15;
/// The page flag marking a page `OS_Memory` 23 reserved.
pub const RESERVED: u32 = 1 << 7;

/// Why `OS_Memory` 23 refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReserveError {
    /// A page of the range is past the last.
    Range,
    /// A page of the range is reserved already.
    Reserved,
    /// The host has no memory for the pages' records.
    NoMemory,
}

/// `OS_Memory` 17's bits for what an access privilege allows: User mode's
/// execute, write and read, then the privileged modes'.
#[must_use]
pub fn rights_bits(access: Access) -> u32 {
    let bits = |rights: Rights, execute: bool| -> u32 {
        let (read, write) = match rights {
            Rights::None => (false, false),
            Rights::Read => (true, false),
            Rights::ReadWrite => (true, true),
        };
        u32::from(execute && read) | u32::from(write) << 1 | u32::from(read) << 2
    };
    bits(access.user, access.execute) | bits(access.privileged, access.execute) << 3
}

/// `OS_Memory` 17: access privilege `number`'s permissions, or -1 past the
/// last.
#[must_use]
pub fn privilege_rights(number: u32) -> (u32, u32) {
    match access_privilege(number) {
        Some(access) => (number, rights_bits(access)),
        None => (u32::MAX, 0),
    }
}

/// `OS_Memory` 18: the access privilege whose permissions, under `mask`,
/// are `wanted`, granting the fewest rights beyond — user write worst, then
/// user execute, user read, privileged write, execute and read.
#[must_use]
pub fn find_access_privilege(wanted: u32, mask: u32) -> Option<(u32, u32)> {
    const PENALTY: [u32; 6] = [1 << 1, 1 << 0, 1 << 2, 1 << 4, 1 << 3, 1 << 5];
    let cost = |extra: u32| -> u32 {
        PENALTY
            .iter()
            .enumerate()
            .filter(|&(_, &bit)| extra & bit != 0)
            .map(|(rank, _)| 1 << (PENALTY.len() - rank))
            .sum()
    };
    (0..)
        .map_while(|n| access_privilege(n).map(|access| (n, rights_bits(access))))
        .filter(|&(_, rights)| rights & mask == wanted)
        .min_by_key(|&(_, rights)| cost(rights & !wanted))
}

impl Memory {
    /// The pages the guest has.
    #[must_use]
    pub const fn ceiling_pages(&self) -> u32 {
        self.ceiling.pages()
    }

    /// Where page `page` is mapped and its RISC OS page flags; none for a
    /// page out of range. A free page, or one the kernel keeps, is mapped
    /// nowhere: -1.
    #[must_use]
    pub fn page_map(&self, page: u32) -> Option<(u32, u32)> {
        if page >= self.ceiling.pages() {
            return None;
        }
        let record = self.pages.get(page);
        let marks = if record.unavailable { UNAVAILABLE } else { 0 }
            | if record.reserved { RESERVED } else { 0 };
        let (logical, flags) = match record.holder {
            Some(Holder::Slot { index, .. }) => (APPLICATION + index * PAGE_SIZE, 0),
            Some(Holder::Area { area, index }) => {
                self.area_page_place(area, index).unwrap_or((u32::MAX, 0))
            }
            Some(Holder::Kernel) => (u32::MAX, 0),
            None => (u32::MAX, super::areas::FREE_POOL_FLAGS),
        };
        Some((logical, flags | marks))
    }

    /// The page the current task sees at `address`, if one is mapped there.
    #[must_use]
    pub fn page_at(&self, address: u32) -> Option<u32> {
        let (frame, _) = self.mmu.lookup(self.mmu.table_of(address), address)?;
        Some(frame.number())
    }

    /// `OS_SetMemMapEntries` for page `page`: its access changed to `flags`'
    /// privilege where it is mapped at `logical`. A page is never moved to
    /// another's place, so a move is not made.
    pub fn set_page_map(&mut self, page: u32, logical: u32, flags: u32) {
        let Some((at, _)) = self.page_map(page) else {
            return;
        };
        if at != logical {
            return;
        }
        if let Some(access) = access_privilege(flags & 0xF) {
            let table = self.mmu.table_of(logical);
            let _ = self.mmu.protect(table, logical, access);
        }
    }

    /// `OS_Memory` 12: the first of `pages` free pages in a row, none
    /// claimed exclusively or reserved, starting on a physical boundary of
    /// two to the `align_bits` bytes and lying within the physical addresses
    /// `low` to `high` inclusive. A page's physical address is its number
    /// times the page size.
    #[must_use]
    pub fn contiguous_free(&self, pages: u32, align_bits: u32, low: u64, high: u64) -> Option<u32> {
        let page_size = u64::from(PAGE_SIZE);
        let pages = pages.max(1);
        let step = 1u32.checked_shl(align_bits.saturating_sub(PAGE_SIZE.trailing_zeros()))?;
        let total = (high.saturating_add(1) / page_size).min(u64::from(self.ceiling.pages()));
        let total = u32::try_from(total).ok()?;
        let mut start = u32::try_from(low.div_ceil(page_size))
            .ok()?
            .checked_next_multiple_of(step)?;
        while let Some(end) = start.checked_add(pages).filter(|&end| end <= total) {
            // Past the window's last taken page, no start before it can do.
            match (start..end).rev().find(|&page| self.page_taken(page)) {
                None => return Some(start),
                Some(taken) => start = (taken + 1).checked_next_multiple_of(step)?,
            }
        }
        None
    }

    /// Whether page `page` is in use, claimed exclusively or reserved.
    fn page_taken(&self, page: u32) -> bool {
        let record = self.pages.get(page);
        !self.mmu.is_free(Frame::new(page)) || record.unavailable || record.reserved
    }

    /// `OS_Memory` 23: reserves `count` pages from `first`, refusing if any
    /// is reserved already and changing none, or releases them.
    pub fn reserve_pages(
        &mut self,
        first: u32,
        count: u32,
        reserve: bool,
    ) -> Result<(), ReserveError> {
        let end = first
            .checked_add(count)
            .filter(|&end| end <= self.ceiling.pages() || count == 0)
            .ok_or(ReserveError::Range)?;
        if reserve && (first..end).any(|page| self.pages.get(page).reserved) {
            return Err(ReserveError::Reserved);
        }
        if count > 0 {
            self.pages
                .get_mut(end - 1)
                .map_err(|_| ReserveError::NoMemory)?;
        }
        for page in first..end {
            if let Ok(record) = self.pages.get_mut(page) {
                record.reserved = reserve;
            }
        }
        Ok(())
    }

    /// Whether page `page` is reserved.
    #[must_use]
    pub fn page_reserved(&self, page: u32) -> bool {
        self.pages.get(page).reserved
    }

    /// Whether page `page` is claimed exclusively.
    #[must_use]
    pub fn page_unavailable(&self, page: u32) -> bool {
        self.pages.get(page).unavailable
    }
}
