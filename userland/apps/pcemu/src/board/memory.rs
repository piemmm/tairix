//! The 1 MiB physical address space: 256 pages of 4 KiB, each a window onto
//! the backing store or the open bus.
//!
//! All RAM and ROM lives in one store, and a page is an offset into it, so
//! moving memory around the address space is a page-table update. A banked
//! range — an expanded-memory frame — is held for its board whether or not
//! it shows anything, and shows store the address space does not otherwise
//! reach.
//!
//! The store is watched in granules wherever translated code was read from
//! it. A granule written by anything, or no longer shown where it was, goes
//! stale once — whatever the address it was reached through, since a bank
//! can show one page in two windows — for the code cache to drop the blocks
//! over it.

use crate::cpu::jit::cache::Watch;
// The store is watched in pages of the page map translated code reaches it
// through.
use crate::cpu::jit::page::PAGE_BYTES as GRANULE;
use crate::cpu::jit::Code;
use crate::cpu::OPEN_BUS;
use alloc::vec::Vec;
use core::ops::Range;

/// Bytes in a page.
pub const PAGE_SIZE: u32 = 0x1000;
/// Pages in the space.
pub const PAGES: usize = 256;
/// The top of the address space.
const SPACE: u32 = 0x10_0000;

/// A page's attributes.
mod attr {
    pub const READ: u8 = 0x01;
    pub const WRITE: u8 = 0x02;
    /// The emulator's own ROM, where a host call stops the CPU.
    pub const HOST: u8 = 0x04;
    /// The 80186's control block is mapped somewhere in the page.
    pub const OVERLAY: u8 = 0x08;
    /// A card's memory: the page's offset is the card's number.
    pub const DEVICE: u8 = 0x10;
    /// A bank's frame, its board's whatever it shows.
    pub const BANKED: u8 = 0x20;
    /// The debugger watches cycles on the page.
    pub const PROBED: u8 = 0x40;
    /// What keeps a page from being claimed again.
    pub const HELD: u8 = READ | WRITE | DEVICE | BANKED;
    /// What a page keeps whatever is mapped into it.
    pub const KEPT: u8 = OVERLAY | PROBED;
    /// What makes every access to a page the board's to see.
    pub const BOARD: u8 = DEVICE | OVERLAY | PROBED;
}

#[derive(Clone, Copy, Debug)]
struct Page {
    offset: u32,
    attr: u8,
}

const UNMAPPED: Page = Page { offset: 0, attr: 0 };

/// Why a mapping was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// The range is empty, not whole pages, or runs past 1 MiB.
    Misaligned,
    /// Part of the range is already mapped.
    Occupied,
    /// The store could not grow.
    OutOfMemory,
}

/// The address space.
#[derive(Clone, Debug)]
pub struct Memory {
    store: Vec<u8>,
    pages: [Page; PAGES],
    /// Each granule of the store translated code was read from: a bit each.
    watched: Vec<u64>,
    /// Granules written, or no longer shown where they were, since they
    /// were watched: each once.
    stale: Vec<u32>,
    /// Each page whose entries in the page map must be made again — what it
    /// shows, or whether translated code may store there, changed: a bit
    /// each.
    dirty: [u64; PAGES / 64],
}

impl Memory {
    /// An address space with `conventional` bytes of RAM from address 0 and
    /// nothing else mapped.
    pub fn new(conventional: u32) -> Result<Self, MapError> {
        let mut memory = Self {
            store: Vec::new(),
            pages: [UNMAPPED; PAGES],
            watched: Vec::new(),
            stale: Vec::new(),
            dirty: [0; PAGES / 64],
        };
        memory.map_ram(0, conventional)?;
        Ok(memory)
    }

    /// The pages `[base, base + len)` cover, if they are whole, inside the
    /// space and unmapped.
    fn claim(&self, base: u32, len: u32) -> Result<Range<usize>, MapError> {
        let end = base
            .checked_add(len)
            .filter(|&end| end <= SPACE)
            .ok_or(MapError::Misaligned)?;
        if len == 0 || !base.is_multiple_of(PAGE_SIZE) || !len.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Misaligned);
        }
        let pages = page(base)..page(end);
        if self.pages[pages.clone()]
            .iter()
            .any(|p| p.attr & attr::HELD != 0)
        {
            return Err(MapError::Occupied);
        }
        Ok(pages)
    }

    /// Grows the store by `len` bytes, `bytes` then zeros, returning where
    /// they start.
    fn grow(&mut self, bytes: &[u8], len: u32) -> Result<u32, MapError> {
        let offset = u32::try_from(self.store.len()).map_err(|_| MapError::OutOfMemory)?;
        let len = len as usize;
        self.store
            .try_reserve_exact(len)
            .map_err(|_| MapError::OutOfMemory)?;
        self.store.extend_from_slice(bytes);
        self.store.resize(offset as usize + len, 0);
        self.watched
            .resize(self.store.len().div_ceil(GRANULE as usize * 64), 0);
        // The store may have moved, and every page with it.
        self.dirty = [u64::MAX; PAGES / 64];
        Ok(offset)
    }

    fn map(&mut self, pages: Range<usize>, offset: u32, attributes: u8) {
        for at in pages.clone() {
            self.changed(at);
        }
        for (i, slot) in self.pages[pages].iter_mut().enumerate() {
            let i = u32::try_from(i).unwrap_or(0);
            *slot = Page {
                offset: offset + i * PAGE_SIZE,
                attr: attributes | (slot.attr & attr::KEPT),
            };
        }
    }

    /// Maps `len` bytes of zeroed RAM at `base`.
    pub fn map_ram(&mut self, base: u32, len: u32) -> Result<(), MapError> {
        let pages = self.claim(base, len)?;
        let offset = self.grow(&[], len)?;
        self.map(pages, offset, attr::READ | attr::WRITE);
        Ok(())
    }

    /// Adds `len` bytes of zeroed RAM to the store that no page shows yet;
    /// returns where they start.
    pub fn add_banked(&mut self, len: u32) -> Result<u32, MapError> {
        self.grow(&[], len)
    }

    /// Holds `[base, base + len)` as a bank's frame, showing nothing.
    pub fn reserve_frame(&mut self, base: u32, len: u32) -> Result<(), MapError> {
        let pages = self.claim(base, len)?;
        self.map(pages, 0, attr::BANKED);
        Ok(())
    }

    /// Shows `len` bytes of banked store from `offset` at `base` in a frame,
    /// or nothing; pages outside every frame are left alone.
    pub fn show_banked(&mut self, base: u32, len: u32, offset: Option<u32>) {
        let end = base.saturating_add(len).min(SPACE);
        let start = page(base.min(SPACE));
        for (i, at) in (0u32..).zip(start..page(end)) {
            let slot = self.pages[at];
            if slot.attr & attr::BANKED == 0 {
                continue;
            }
            if slot.attr & attr::READ != 0 {
                self.touched(slot.offset..slot.offset + PAGE_SIZE);
            }
            self.changed(at);
            self.pages[at] = match offset {
                Some(offset) => Page {
                    offset: offset + i * PAGE_SIZE,
                    attr: attr::BANKED | attr::READ | attr::WRITE | (slot.attr & attr::KEPT),
                },
                None => Page {
                    offset: 0,
                    attr: attr::BANKED | (slot.attr & attr::KEPT),
                },
            };
        }
    }

    /// The store's bytes `[offset, offset + len)`, if it holds them; any of
    /// them translated code was read from goes stale, since the caller may
    /// write them.
    pub fn store_mut(&mut self, offset: u32, len: u32) -> Option<&mut [u8]> {
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(usize::try_from(len).ok()?)?;
        if end > self.store.len() {
            return None;
        }
        self.touched(offset..offset.saturating_add(len));
        self.store.get_mut(start..end)
    }

    /// Maps `image` as ROM at `base`, padded to whole pages with zeros.
    /// `host` marks it the emulator's own, where host calls are honoured.
    pub fn map_rom(&mut self, base: u32, image: &[u8], host: bool) -> Result<(), MapError> {
        let len = u32::try_from(image.len())
            .ok()
            .and_then(|len| len.checked_next_multiple_of(PAGE_SIZE))
            .ok_or(MapError::Misaligned)?;
        let pages = self.claim(base, len)?;
        let offset = self.grow(image, len)?;
        self.map(
            pages,
            offset,
            attr::READ | if host { attr::HOST } else { 0 },
        );
        Ok(())
    }

    fn slot(&self, addr: u32) -> Page {
        self.pages[page(addr & (SPACE - 1))]
    }

    /// Reads the byte at `addr`.
    #[inline]
    #[must_use]
    pub fn read(&self, addr: u32) -> u8 {
        let page = self.slot(addr);
        if page.attr & attr::READ == 0 {
            return OPEN_BUS;
        }
        let at = page.offset + (addr & (PAGE_SIZE - 1));
        self.store.get(at as usize).copied().unwrap_or(OPEN_BUS)
    }

    /// Writes the byte at `addr`; ROM and the open bus ignore it.
    #[inline]
    pub fn write(&mut self, addr: u32, value: u8) {
        let page = self.slot(addr);
        if page.attr & attr::WRITE == 0 {
            return;
        }
        let at = page.offset + (addr & (PAGE_SIZE - 1));
        if let Some(byte) = self.store.get_mut(at as usize) {
            *byte = value;
            self.touched(at..at + 1);
        }
    }

    /// Gives card `device` the pages of `[base, base + len)` that nothing
    /// else holds; a page another card or memory already answers stays with
    /// it, as the first to decode an address keeps it on the bus. Returns
    /// whether every page was free.
    pub fn map_device(&mut self, base: u32, len: u32, device: usize) -> bool {
        let Ok(number) = u32::try_from(device) else {
            return false;
        };
        let end = base.saturating_add(len).min(SPACE);
        let mut every = true;
        for at in page(base.min(SPACE))..page(end) {
            let slot = self.pages[at];
            if slot.attr & attr::HELD == 0 {
                self.pages[at] = Page {
                    offset: number,
                    attr: attr::DEVICE | (slot.attr & attr::KEPT),
                };
                self.changed(at);
            } else {
                every = false;
            }
        }
        every
    }

    /// Takes back every page card `device` holds.
    pub fn unmap_device(&mut self, device: usize) {
        for at in 0..PAGES {
            let slot = self.pages[at];
            if slot.attr & attr::DEVICE != 0 && slot.offset as usize == device {
                self.pages[at] = Page {
                    offset: 0,
                    attr: slot.attr & attr::KEPT,
                };
                self.changed(at);
            }
        }
    }

    /// Whether `addr` needs the board's attention: a card's memory, the
    /// page the 80186's control block overlays, or a page the debugger
    /// watches.
    #[inline]
    #[must_use]
    pub fn special(&self, addr: u32) -> bool {
        self.slot(addr).attr & attr::BOARD != 0
    }

    /// Whether the debugger watches the page holding `addr`.
    #[inline]
    #[must_use]
    pub fn probed(&self, addr: u32) -> bool {
        self.slot(addr).attr & attr::PROBED != 0
    }

    /// Marks each page the debugger watches as `watched` says, and the
    /// others as unwatched.
    pub fn set_probed(&mut self, watched: impl Fn(usize) -> bool) {
        for at in 0..PAGES {
            let probed = watched(at);
            if (self.pages[at].attr & attr::PROBED != 0) != probed {
                self.changed(at);
                self.pages[at].attr ^= attr::PROBED;
            }
        }
    }

    /// The card whose memory `addr` is, if any.
    #[inline]
    #[must_use]
    pub fn device(&self, addr: u32) -> Option<usize> {
        let page = self.slot(addr);
        (page.attr & attr::DEVICE != 0).then_some(page.offset as usize)
    }

    /// Whether `addr` is in the emulator's own ROM.
    #[inline]
    #[must_use]
    pub fn host_code(&self, addr: u32) -> bool {
        self.slot(addr).attr & attr::HOST != 0
    }

    /// Whether `addr` is in the page the 80186's control block overlays.
    #[inline]
    #[must_use]
    pub fn overlaid(&self, addr: u32) -> bool {
        self.slot(addr).attr & attr::OVERLAY != 0
    }

    /// Marks the page holding `addr`, and only that page, as overlaid; or
    /// none.
    pub fn set_overlay(&mut self, addr: Option<u32>) {
        for at in 0..PAGES {
            let slot = self.pages[at];
            let overlaid = addr.is_some_and(|addr| page(addr & (SPACE - 1)) == at);
            if (slot.attr & attr::OVERLAY != 0) != overlaid {
                // Code read from a page the control block now covers, or no
                // longer covers, is not what fetching there now reads.
                if slot.attr & attr::READ != 0 {
                    self.touched(slot.offset..slot.offset + PAGE_SIZE);
                }
                self.changed(at);
            }
            self.pages[at].attr = if overlaid {
                slot.attr | attr::OVERLAY
            } else {
                slot.attr & !attr::OVERLAY
            };
        }
    }

    /// The store offset of the byte `addr` shows from memory, where code
    /// may be read from it and translated: RAM or ROM, neither a card's, nor
    /// overlaid, nor watched.
    #[must_use]
    pub fn backing(&self, addr: u32) -> Option<u32> {
        let page = self.slot(addr);
        (page.attr & attr::READ != 0 && page.attr & attr::BOARD == 0)
            .then(|| page.offset + (addr & (PAGE_SIZE - 1)))
    }

    /// Whether `addr` shows memory a store may be made to: RAM, neither a
    /// card's, nor overlaid, nor watched.
    #[must_use]
    pub fn writable(&self, addr: u32) -> bool {
        let page = self.slot(addr);
        page.attr & attr::WRITE != 0 && page.attr & attr::BOARD == 0
    }

    /// The host address of the store's byte at `offset`: only ever
    /// compared and offset from, never dereferenced here.
    #[must_use]
    pub fn host_address(&self, offset: u32) -> u64 {
        let base = u64::try_from(self.store.as_ptr().addr()).unwrap_or(0);
        base.wrapping_add(u64::from(offset))
    }

    /// Whether granule `granule` is watched.
    #[must_use]
    pub fn is_watched(&self, granule: u32) -> bool {
        self.watched
            .get((granule / 64) as usize)
            .is_some_and(|word| word >> (granule % 64) & 1 != 0)
    }

    /// Every watched granule stale: the store was replaced whole.
    pub fn replaced(&mut self) {
        let len = u32::try_from(self.store.len()).unwrap_or(u32::MAX);
        self.touched(0..len);
    }

    /// The whole store, for translated code to reach through the page map,
    /// which keeps its stores off what is watched: nothing goes stale.
    pub fn store_for_blocks(&mut self) -> &mut [u8] {
        &mut self.store
    }

    /// The pages whose page-map entries must be made again since the last
    /// call: a bit each.
    pub fn take_dirty(&mut self) -> [u64; PAGES / 64] {
        core::mem::take(&mut self.dirty)
    }

    /// The bytes `range` of the store may have changed: each granule of
    /// them watched goes stale.
    #[inline]
    fn touched(&mut self, range: Range<u32>) {
        if range.is_empty() {
            return;
        }
        for granule in range.start / GRANULE..=(range.end - 1) / GRANULE {
            let Some(word) = self.watched.get_mut((granule / 64) as usize) else {
                return;
            };
            if *word >> (granule % 64) & 1 != 0 {
                *word &= !(1 << (granule % 64));
                self.stale.push(granule);
                self.showing_changed(granule);
            }
        }
    }

    /// Page `at` changed what it shows.
    fn changed(&mut self, at: usize) {
        if let Some(word) = self.dirty.get_mut(at / 64) {
            *word |= 1 << (at % 64);
        }
    }

    /// Whether translated code may store into granule `granule` changed:
    /// every page showing it changes.
    fn showing_changed(&mut self, granule: u32) {
        let at = granule * GRANULE;
        for page in 0..PAGES {
            let slot = self.pages[page];
            if slot.attr & attr::READ != 0 && (slot.offset..slot.offset + PAGE_SIZE).contains(&at) {
                self.changed(page);
            }
        }
    }
}

impl Code for Memory {
    fn byte(&self, addr: u32) -> Option<u8> {
        (self.backing(addr).is_some() && !self.host_code(addr)).then(|| self.read(addr))
    }
}

impl Watch for Memory {
    fn granule(&self, addr: u32) -> Option<u32> {
        self.backing(addr).map(|offset| offset / GRANULE)
    }

    fn watch(&mut self, granule: u32) {
        if let Some(word) = self.watched.get_mut((granule / 64) as usize) {
            if *word >> (granule % 64) & 1 == 0 {
                *word |= 1 << (granule % 64);
                self.showing_changed(granule);
            }
        }
    }

    fn unwatch(&mut self, granule: u32) {
        if let Some(word) = self.watched.get_mut((granule / 64) as usize) {
            if *word >> (granule % 64) & 1 != 0 {
                *word &= !(1 << (granule % 64));
                self.showing_changed(granule);
            }
        }
    }

    fn take_stale(&mut self) -> Vec<u32> {
        core::mem::take(&mut self.stale)
    }
}

/// The page an address in the space falls in.
fn page(addr: u32) -> usize {
    (addr / PAGE_SIZE) as usize
}

// What each page shows follows from the devices that map it, so the board
// shows them again once they are restored; the watch is the code cache's,
// which the restorer tells.
crate::snapshot::state!(Memory { store } as built { pages, watched, stale, dirty });

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::jit::cache::Watch;
    use crate::cpu::jit::Code;

    #[test]
    fn ram_reads_back_and_the_rest_floats() {
        let mut memory = Memory::new(0x1_0000).unwrap();
        memory.write(0xFFFF, 0x5A);
        assert_eq!(memory.read(0xFFFF), 0x5A);
        memory.write(0x1_0000, 0x5A);
        assert_eq!(memory.read(0x1_0000), OPEN_BUS);
    }

    #[test]
    fn rom_ignores_writes_and_marks_host_code() {
        let mut memory = Memory::new(0x1_0000).unwrap();
        memory.map_rom(0xF_E000, &[0xEA, 0x5B, 0xE0], true).unwrap();
        memory.write(0xF_E000, 0);
        assert_eq!(memory.read(0xF_E000), 0xEA);
        assert_eq!(memory.read(0xF_EFFF), 0, "padded to whole pages");
        assert_eq!(memory.read(0xF_F000), OPEN_BUS);
        assert!(memory.host_code(0xF_E002));
        assert!(!memory.host_code(0xF_D000));
    }

    #[test]
    fn mappings_must_be_whole_free_pages_inside_the_space() {
        let mut memory = Memory::new(0xA_0000).unwrap();
        assert_eq!(
            memory.map_rom(0x9_F000, &[0; 16], false),
            Err(MapError::Occupied)
        );
        assert_eq!(
            memory.map_rom(0xC_0800, &[0; 16], false),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            memory.map_rom(0xF_F000, &[0; 0x2000], false),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            memory.map_rom(0xC_0000, &[], false),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            memory.map_ram(0xFFFF_F000, 0x1000),
            Err(MapError::Misaligned)
        );
    }

    #[test]
    fn a_frame_shows_banked_store_by_page_table_and_nothing_else_claims_it() {
        let mut memory = Memory::new(0x1_0000).unwrap();
        let bank = memory.add_banked(2 * 0x4000).unwrap();
        memory.reserve_frame(0xD_0000, 0x1_0000).unwrap();
        assert_eq!(memory.read(0xD_0000), OPEN_BUS, "nothing shown yet");
        memory.write(0xD_0000, 0x11);
        assert_eq!(
            memory.map_rom(0xD_4000, &[0; 16], false),
            Err(MapError::Occupied)
        );
        assert!(!memory.map_device(0xD_C000, 0x4000, 0), "no card takes it");
        memory.show_banked(0xD_4000, 0x4000, Some(bank + 0x4000));
        memory.write(0xD_4005, 0x5A);
        assert_eq!(memory.store_mut(bank + 0x4005, 1), Some(&mut [0x5A][..]));
        memory.show_banked(0xD_8000, 0x4000, Some(bank + 0x4000));
        assert_eq!(
            memory.read(0xD_8005),
            0x5A,
            "one page seen through two windows"
        );
        memory.show_banked(0xD_4000, 0x4000, None);
        assert_eq!(memory.read(0xD_4005), OPEN_BUS);
        memory.show_banked(0x0000, 0x4000, Some(bank));
        memory.write(0x0005, 0x77);
        assert_ne!(
            memory.store_mut(bank + 5, 1),
            Some(&mut [0x77][..]),
            "RAM is no frame"
        );
        assert_eq!(memory.store_mut(bank, 0x9000), None, "past the store");
    }

    /// A memory with two 16 KiB banks behind a frame at `D0000h`, the
    /// second bank shown in two windows, and the granule of the first byte
    /// it shows there watched.
    fn aliased() -> (Memory, u32) {
        let mut memory = Memory::new(0x1_0000).unwrap();
        let bank = memory.add_banked(2 * 0x4000).unwrap();
        memory.reserve_frame(0xD_0000, 0x1_0000).unwrap();
        memory.show_banked(0xD_0000, 0x4000, Some(bank + 0x4000));
        memory.show_banked(0xD_8000, 0x4000, Some(bank + 0x4000));
        let granule = memory.granule(0xD_0000).unwrap();
        memory.watch(granule);
        (memory, granule)
    }

    #[test]
    fn a_watched_granule_written_through_any_window_goes_stale_once() {
        let (mut memory, granule) = aliased();
        assert_eq!(
            memory.granule(0xD_8000),
            Some(granule),
            "one store, two windows"
        );
        memory.write(0xD_8005, 1);
        memory.write(0xD_0006, 2);
        assert_eq!(memory.take_stale(), [granule]);
        assert!(!memory.is_watched(granule));
        memory.write(0xD_0005, 3);
        assert!(memory.take_stale().is_empty(), "unwatched now");
        memory.watch(granule);
        memory.write(0xD_00FF, 4);
        memory.write(0xD_0100, 5);
        assert_eq!(
            memory.take_stale(),
            [granule],
            "the granule's last byte; the next is another's"
        );
    }

    #[test]
    fn what_could_change_the_bytes_makes_them_stale() {
        let (mut memory, granule) = aliased();
        let offset = memory.backing(0xD_0000).unwrap();
        assert!(memory.store_mut(offset + 0x10, 1).is_some());
        assert_eq!(memory.take_stale(), [granule], "the store handed out");

        let (mut memory, granule) = aliased();
        memory.show_banked(0xD_0000, 0x4000, None);
        assert_eq!(
            memory.take_stale(),
            [granule],
            "no longer shown where it was read"
        );

        let (mut memory, granule) = aliased();
        memory.set_overlay(Some(0xD_8000));
        assert_eq!(memory.take_stale(), [granule], "the control block over it");

        let (mut memory, granule) = aliased();
        memory.replaced();
        assert_eq!(memory.take_stale(), [granule], "the store replaced");

        let (mut memory, _) = aliased();
        memory.write(0xD_4000, 1);
        assert!(memory.store_mut(0, 0x100).is_some());
        assert!(memory.take_stale().is_empty(), "other bytes");
    }

    #[test]
    fn code_is_read_only_from_memory_a_block_may_be_translated_from() {
        let mut memory = Memory::new(0x1_0000).unwrap();
        memory.map_rom(0xF_E000, &[0xEA], true).unwrap();
        memory.map_rom(0xC_0000, &[0x55], false).unwrap();
        memory.write(0x100, 0x90);
        assert_eq!(memory.byte(0x100), Some(0x90));
        assert_eq!(memory.byte(0xC_0000), Some(0x55), "a card's ROM");
        assert_eq!(
            memory.byte(0xF_E000),
            None,
            "the emulator's own, with its host calls"
        );
        assert_eq!(memory.byte(0x2_0000), None, "nothing there");
        assert!(memory.map_device(0xB_8000, 0x8000, 0));
        assert_eq!(memory.byte(0xB_8000), None, "a card's memory");
        assert_eq!(memory.granule(0xB_8000), None);
    }

    #[test]
    fn the_overlay_marks_one_page() {
        let mut memory = Memory::new(0x1_0000).unwrap();
        memory.set_overlay(Some(0x1_2300));
        assert!(memory.overlaid(0x1_2000));
        assert!(memory.overlaid(0x1_2FFF));
        assert!(!memory.overlaid(0x1_3000));
        memory.set_overlay(None);
        assert!(!memory.overlaid(0x1_2300));
    }
}
