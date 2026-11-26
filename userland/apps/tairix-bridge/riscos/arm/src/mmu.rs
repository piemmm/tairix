//! The guest MMU (`plans/RISCOS-EMULATOR.md` §3): the frames guest memory is
//! made of, the page tables that map them, the software TLB in front of the
//! tables, and the write watch.
//!
//! - **Frames** are 4 KiB physical pages numbered from zero, at most
//!   [`Mmu::limit`] of them in use. A frame lives while it is held or mapped
//!   and starts zeroed. Allocation takes the lowest free number, so a run's
//!   numbering follows from what it asked for alone.
//! - **Spaces.** Below the current space's extent that space's own table
//!   answers; above it the global table does. A switch swaps one pointer
//!   whatever the spaces hold, and a TLB entry carries the space it was found
//!   in, so a switch flushes nothing. No global page lies below any space's
//!   extent, so no space can hide one.
//! - **Access.** A page grants User mode and the privileged modes each nothing,
//!   reading, or reading and writing, and may be executable. An access its page
//!   does not grant is a permission abort; one where nothing is mapped is a
//!   translation abort.
//! - **Host frames** hold the emulator's own code: no mapping lets the guest
//!   write one, and an instruction fetched from one says so.
//! - **The watch.** A store into a watched 256-byte granule reports it to each
//!   watcher watching it, once until that watcher takes its reports. A frame
//!   written by the host, or freed, reports every granule watched in it.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_collections::{BitVec, OrderedMap};

use crate::bus::{Abort, Bus, Fetch, Width};

/// Bytes in a page.
pub const PAGE_SIZE: u32 = 1 << PAGE_BITS;
/// The bits of an address within its page.
pub const PAGE_BITS: u32 = 12;

/// The bytes from `address` to the end of its page.
#[must_use]
pub const fn to_page_end(address: u32) -> u32 {
    PAGE_SIZE - address % PAGE_SIZE
}
/// Bytes in a granule, the unit the watch reports.
pub const GRANULE_SIZE: u32 = 1 << GRANULE_BITS;
const GRANULE_BITS: u32 = 8;
/// Words in a frame.
pub const FRAME_WORDS: usize = (PAGE_SIZE / 4) as usize;
/// The parties that watch stores independently: the display's damage and the
/// translator's code.
pub const WATCHERS: usize = 2;
/// Pages one table maps: 4 MiB.
const TABLE_BITS: u32 = 10;
const TABLE_PAGES: usize = 1 << TABLE_BITS;
/// Bytes one table maps: [`Mmu::reserve`] makes tables a span at a time.
pub const TABLE_SPAN: u32 = PAGE_SIZE << TABLE_BITS;
/// The tables of the whole 32-bit space.
const TABLES: usize = 1 << (32 - PAGE_BITS - TABLE_BITS);
/// The most frames the MMU names: a page-table entry holds a frame's number
/// in the bits above its flags.
pub const MOST_FRAMES: u32 = 1 << (32 - pte::FRAME_SHIFT);
/// TLB entries, a page's chosen by its number. Large enough that a program's
/// working set rarely collides, small enough to stay in a core's cache.
const TLB_ENTRIES: usize = 1024;

/// A frame: a physical page, by number.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Frame(u32);

impl Frame {
    /// The frame numbered `number`.
    #[must_use]
    pub const fn new(number: u32) -> Self {
        Self(number)
    }

    /// Its number.
    #[must_use]
    pub const fn number(self) -> u32 {
        self.0
    }

    const fn index(self) -> usize {
        self.0 as usize
    }
}

/// What a frame holds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FrameKind {
    /// Guest memory.
    Memory,
    /// The emulator's own code, where a host call is honoured.
    Host,
}

/// What one privilege may do to a page.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Rights {
    /// Nothing.
    None,
    /// Read it.
    Read,
    /// Read and write it.
    ReadWrite,
}

impl Rights {
    const fn readable(self) -> bool {
        !matches!(self, Self::None)
    }

    const fn writable(self) -> bool {
        matches!(self, Self::ReadWrite)
    }

    const fn bits(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Read => 1,
            Self::ReadWrite => 3,
        }
    }

    const fn from_bits(bits: u32) -> Self {
        match bits & 3 {
            0 => Self::None,
            1 => Self::Read,
            _ => Self::ReadWrite,
        }
    }
}

/// A page's permissions.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Access {
    /// What User mode may do.
    pub user: Rights,
    /// What the privileged modes may do.
    pub privileged: Rights,
    /// Whether code may be fetched from it, by a mode that may read it.
    pub execute: bool,
}

/// The accesses a TLB entry grants, a bit each.
mod allow {
    pub const USER_READ: u8 = 1 << 0;
    pub const USER_WRITE: u8 = 1 << 1;
    pub const USER_EXECUTE: u8 = 1 << 2;
    pub const PRIVILEGED_READ: u8 = 1 << 3;
    pub const PRIVILEGED_WRITE: u8 = 1 << 4;
    pub const PRIVILEGED_EXECUTE: u8 = 1 << 5;
}

/// A page-table entry: the frame above the low byte, the access in its low
/// five bits.
mod pte {
    pub const ACCESS: u32 = 0x1F;
    pub const EXECUTE: u32 = 1 << 4;
    pub const PRESENT: u32 = 1 << 7;
    pub const FRAME_SHIFT: u32 = 8;
}

impl Access {
    const fn bits(self) -> u32 {
        let execute = if self.execute { pte::EXECUTE } else { 0 };
        self.user.bits() | (self.privileged.bits() << 2) | execute
    }

    const fn from_bits(bits: u32) -> Self {
        Self {
            user: Rights::from_bits(bits),
            privileged: Rights::from_bits(bits >> 2),
            execute: bits & pte::EXECUTE != 0,
        }
    }

    /// The accesses it grants, as the TLB tests them.
    const fn allows(self) -> u8 {
        let mut allows = 0;
        if self.user.readable() {
            allows |= allow::USER_READ;
            if self.execute {
                allows |= allow::USER_EXECUTE;
            }
        }
        if self.user.writable() {
            allows |= allow::USER_WRITE;
        }
        if self.privileged.readable() {
            allows |= allow::PRIVILEGED_READ;
            if self.execute {
                allows |= allow::PRIVILEGED_EXECUTE;
            }
        }
        if self.privileged.writable() {
            allows |= allow::PRIVILEGED_WRITE;
        }
        allows
    }
}

/// The TLB bit an access of `kind` needs at `privileged`.
const fn wanted(kind: Kind, privileged: bool) -> u8 {
    match (kind, privileged) {
        (Kind::Read, false) => allow::USER_READ,
        (Kind::Write, false) => allow::USER_WRITE,
        (Kind::Execute, false) => allow::USER_EXECUTE,
        (Kind::Read, true) => allow::PRIVILEGED_READ,
        (Kind::Write, true) => allow::PRIVILEGED_WRITE,
        (Kind::Execute, true) => allow::PRIVILEGED_EXECUTE,
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Read,
    Write,
    Execute,
}

/// An address space below its extent: a task's, in the session's terms. A
/// handle outliving its space names nothing, whatever is made after it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Space {
    index: u32,
    generation: u32,
}

impl Space {
    const fn index(self) -> usize {
        self.index as usize
    }

    /// The tag its TLB entries carry; the global table's is zero.
    const fn tag(self) -> u32 {
        self.index + 1
    }
}

/// The table a page is mapped in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Table {
    /// The global table, seen above every space's extent.
    Global,
    /// A space's own, seen below its extent while it is current.
    Space(Space),
}

/// One of the parties that watch stores, each independently of the others.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Watcher(usize);

impl Watcher {
    /// The watcher numbered `index`, if there is one.
    #[must_use]
    pub const fn new(index: usize) -> Option<Self> {
        if index < WATCHERS {
            Some(Self(index))
        } else {
            None
        }
    }
}

/// Why the MMU refused a request.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MmuError {
    /// No frame is free under the limit, or the host had no memory to give.
    Exhausted,
    /// The frame is not in use; or, to allocate it, already is.
    Frame,
    /// The address is not a page's, or lies where the call may not map: a
    /// space's page at or above its extent, or a global page below any
    /// space's.
    Placement,
    /// A page is mapped there.
    Occupied,
    /// No page is mapped there.
    Unmapped,
    /// A mapping would let the guest write a host frame.
    HostWritable,
    /// No such space, or the current space where another is needed.
    Space,
}

/// A frame's state.
struct Slot {
    words: Option<Box<[u32; FRAME_WORDS]>>,
    /// The page-table entries that name it.
    maps: u32,
    /// Whether its owner holds it, so that it outlives its last mapping.
    held: bool,
    host: bool,
    /// The granules a store reports: watched with no report pending.
    armed: u16,
}

const FREE_SLOT: Slot = Slot {
    words: None,
    maps: 0,
    held: false,
    host: false,
    armed: 0,
};

/// The frames, and which are free.
struct Frames {
    slots: Vec<Slot>,
    /// Set where a slot is free; numbers past the slots are free too.
    free: BitVec,
    limit: u32,
    used: u32,
}

impl Frames {
    fn slot(&self, frame: Frame) -> Option<&Slot> {
        self.slots
            .get(frame.index())
            .filter(|slot| slot.words.is_some())
    }

    fn slot_mut(&mut self, frame: Frame) -> Option<&mut Slot> {
        self.slots
            .get_mut(frame.index())
            .filter(|slot| slot.words.is_some())
    }

    /// The lowest free number.
    fn lowest_free(&self) -> usize {
        self.free.next_one(0).unwrap_or(self.slots.len())
    }

    /// Refuses `frame` taken into use: past the limit, or in use already.
    fn check_take(&self, frame: Frame) -> Result<(), MmuError> {
        let index = frame.index();
        if index < self.slots.len() && self.free.get(index) != Some(true) {
            return Err(MmuError::Frame);
        }
        if frame.0 >= self.limit || self.used >= self.limit {
            return Err(MmuError::Exhausted);
        }
        Ok(())
    }

    /// Takes `frame` into use, held: refused past the limit, or if it is in
    /// use already.
    fn take(&mut self, frame: Frame, kind: FrameKind) -> Result<(), MmuError> {
        self.check_take(frame)?;
        let index = frame.index();
        let words = zeroed().ok_or(MmuError::Exhausted)?;
        if index >= self.slots.len() {
            let len = index + 1;
            self.slots
                .try_reserve(len - self.slots.len())
                .map_err(|_| MmuError::Exhausted)?;
            self.free
                .try_resize(len, true)
                .map_err(|_| MmuError::Exhausted)?;
            self.slots.resize_with(len, || FREE_SLOT);
        }
        self.slots[index] = Slot {
            words: Some(words),
            maps: 0,
            held: true,
            host: kind == FrameKind::Host,
            armed: 0,
        };
        self.free.set(index, false);
        self.used += 1;
        Ok(())
    }

    /// Frees `frame`, giving its memory back.
    fn put(&mut self, frame: Frame) {
        if let Some(slot) = self.slots.get_mut(frame.index()) {
            if slot.words.is_some() {
                *slot = FREE_SLOT;
                self.free.set(frame.index(), true);
                self.used -= 1;
            }
        }
    }
}

/// A zeroed array on the heap, or `None` if the host has no memory for it.
fn zeroed<const N: usize>() -> Option<Box<[u32; N]>> {
    let mut words = Vec::new();
    words.try_reserve_exact(N).ok()?;
    words.resize(N, 0);
    words.into_boxed_slice().try_into().ok()
}

/// The entries of 4 MiB of pages, how many are present, and how many
/// reservations keep it while it maps nothing.
struct Level2 {
    entries: Box<[u32; TABLE_PAGES]>,
    present: u16,
    reservations: u16,
}

/// A table's level-2 tables, made as pages are mapped in them.
struct Tables {
    tables: Vec<Option<Level2>>,
}

impl Tables {
    fn new(count: usize) -> Result<Self, MmuError> {
        let mut tables = Vec::new();
        tables
            .try_reserve_exact(count)
            .map_err(|_| MmuError::Exhausted)?;
        tables.resize_with(count, || None);
        Ok(Self { tables })
    }

    fn entry(&self, page: u32) -> u32 {
        match self.tables.get((page >> TABLE_BITS) as usize) {
            Some(Some(level2)) => level2.entries[page as usize % TABLE_PAGES],
            _ => 0,
        }
    }

    /// Sets an absent entry; the caller has checked the table reaches it.
    fn insert(&mut self, page: u32, entry: u32) -> Result<(), MmuError> {
        let slot = self
            .tables
            .get_mut((page >> TABLE_BITS) as usize)
            .ok_or(MmuError::Placement)?;
        if slot.is_none() {
            *slot = Some(Level2 {
                entries: zeroed().ok_or(MmuError::Exhausted)?,
                present: 0,
                reservations: 0,
            });
        }
        if let Some(level2) = slot {
            level2.entries[page as usize % TABLE_PAGES] = entry;
            level2.present += 1;
        }
        Ok(())
    }

    /// Replaces a present entry.
    fn replace(&mut self, page: u32, entry: u32) {
        if let Some(Some(level2)) = self.tables.get_mut((page >> TABLE_BITS) as usize) {
            level2.entries[page as usize % TABLE_PAGES] = entry;
        }
    }

    /// Clears an entry, giving its level-2 table back once it maps nothing.
    fn remove(&mut self, page: u32) -> u32 {
        let Some(slot) = self.tables.get_mut((page >> TABLE_BITS) as usize) else {
            return 0;
        };
        let Some(level2) = slot else {
            return 0;
        };
        let old = core::mem::take(&mut level2.entries[page as usize % TABLE_PAGES]);
        if old & pte::PRESENT != 0 {
            level2.present -= 1;
            if level2.present == 0 && level2.reservations == 0 {
                *slot = None;
            }
        }
        old
    }

    /// The tables mapping `pages`, by index.
    fn covering(pages: Range<u32>) -> Range<usize> {
        if pages.is_empty() {
            return 0..0;
        }
        (pages.start >> TABLE_BITS) as usize..((pages.end - 1) >> TABLE_BITS) as usize + 1
    }

    /// Makes and keeps the tables mapping `pages`, all or none.
    fn reserve(&mut self, pages: Range<u32>) -> Result<(), MmuError> {
        self.reserve_with(pages, zeroed)
    }

    /// [`Tables::reserve`], each missing table made by `make`: every one is
    /// made before any is kept, so a refusal changes nothing.
    fn reserve_with(
        &mut self,
        pages: Range<u32>,
        mut make: impl FnMut() -> Option<Box<[u32; TABLE_PAGES]>>,
    ) -> Result<(), MmuError> {
        let range = Self::covering(pages);
        let slots = self.tables.get_mut(range).ok_or(MmuError::Placement)?;
        let missing = slots.iter().filter(|slot| slot.is_none()).count();
        let mut fresh = Vec::new();
        fresh
            .try_reserve_exact(missing)
            .map_err(|_| MmuError::Exhausted)?;
        for _ in 0..missing {
            fresh.push(make().ok_or(MmuError::Exhausted)?);
        }
        for slot in slots {
            if slot.is_none() {
                // As many were made as were missing.
                *slot = fresh.pop().map(|entries| Level2 {
                    entries,
                    present: 0,
                    reservations: 0,
                });
            }
            if let Some(level2) = slot {
                level2.reservations += 1;
            }
        }
        Ok(())
    }

    /// Drops a reservation of the tables `indices` names, each kept now only
    /// while it maps something or another reservation holds it.
    fn release(&mut self, indices: Range<usize>) {
        for slot in self.tables.get_mut(indices).into_iter().flatten() {
            if let Some(level2) = slot {
                level2.reservations = level2.reservations.saturating_sub(1);
                if level2.present == 0 && level2.reservations == 0 {
                    *slot = None;
                }
            }
        }
    }

    /// Whether any page in `pages` is present.
    fn any_present(&self, pages: Range<u32>) -> bool {
        let Some(last) = pages.end.checked_sub(1).filter(|&last| last >= pages.start) else {
            return false;
        };
        let first_table = pages.start >> TABLE_BITS;
        (first_table..=last >> TABLE_BITS).any(|index| {
            let Some(Some(level2)) = self.tables.get(index as usize) else {
                return false;
            };
            let base = index << TABLE_BITS;
            let start = pages.start.max(base) - base;
            let end = (last - base).min((1 << TABLE_BITS) - 1);
            level2.entries[start as usize..=end as usize]
                .iter()
                .any(|&entry| entry & pte::PRESENT != 0)
        })
    }

    /// Every present entry, then cleared.
    fn drain(&mut self, mut each: impl FnMut(u32)) {
        for level2 in self.tables.iter_mut().filter_map(Option::take) {
            level2
                .entries
                .iter()
                .filter(|&&entry| entry & pte::PRESENT != 0)
                .for_each(|&entry| each(entry));
        }
    }
}

/// The tables needed to map pages below `extent`.
const fn tables_for(extent: u32) -> usize {
    (extent as usize).div_ceil(1 << (PAGE_BITS + TABLE_BITS))
}

/// A space: its table and its extent.
struct Private {
    tables: Tables,
    extent: u32,
    generation: u32,
}

/// A TLB entry: a page as the space it was looked up in saw it.
#[derive(Clone, Copy)]
struct Entry {
    page: u32,
    tag: u32,
    frame: Frame,
    allows: u8,
    host: bool,
}

/// No page's number is this high, so a vacant entry never matches.
const VACANT: Entry = Entry {
    page: u32::MAX,
    tag: 0,
    frame: Frame(0),
    allows: 0,
    host: false,
};

/// Which granules of a frame each watcher watches, and which of them it has
/// a report pending for.
#[derive(Clone, Copy, Default)]
struct Watch {
    watched: [u16; WATCHERS],
    pending: [u16; WATCHERS],
}

impl Watch {
    fn armed(self) -> u16 {
        self.watched
            .iter()
            .zip(&self.pending)
            .fold(0, |armed, (&watched, &pending)| {
                armed | (watched & !pending)
            })
    }

    fn idle(self) -> bool {
        self.watched
            .iter()
            .chain(&self.pending)
            .all(|&mask| mask == 0)
    }
}

/// Arms `frame` for the granules its watch still waits on, forgetting the
/// watch once nothing is watched or pending.
fn settle(frames: &mut Frames, watches: &mut OrderedMap<u32, Watch>, frame: Frame) {
    let watch = watches.get(&frame.0).copied();
    if watch.is_some_and(Watch::idle) {
        watches.remove(&frame.0);
    }
    if let Some(slot) = frames.slot_mut(frame) {
        slot.armed = watch.map_or(0, Watch::armed);
    }
}

/// The frames and the watch over them.
struct Store {
    frames: Frames,
    watches: OrderedMap<u32, Watch>,
    /// Each watcher's frames with a report pending.
    pending: [BitVec; WATCHERS],
}

impl Store {
    /// Records stores into `granules` of `frame` for each watcher watching
    /// them with no report pending.
    fn report(&mut self, frame: Frame, granules: u16) {
        let Some(watch) = self.watches.get_mut(&frame.0) else {
            return;
        };
        for (w, pending) in self.pending.iter_mut().enumerate() {
            let hits = watch.watched[w] & !watch.pending[w] & granules;
            if hits != 0 {
                if watch.pending[w] == 0 {
                    pending.set(frame.index(), true);
                }
                watch.pending[w] |= hits;
            }
        }
        settle(&mut self.frames, &mut self.watches, frame);
    }

    /// Frees `frame`, reporting every granule watched in it.
    fn free(&mut self, frame: Frame) {
        self.report(frame, u16::MAX);
        if let Some(watch) = self.watches.get_mut(&frame.0) {
            watch.watched = [0; WATCHERS];
            if watch.idle() {
                self.watches.remove(&frame.0);
            }
        }
        self.frames.put(frame);
    }

    /// One mapping of `frame` gone: freed with its last, unless held.
    fn unmapped(&mut self, frame: Frame) {
        let Some(slot) = self.frames.slot_mut(frame) else {
            return;
        };
        slot.maps -= 1;
        if slot.maps == 0 && !slot.held {
            self.free(frame);
        }
    }
}

/// The guest's memory as the processor sees it.
pub struct Mmu {
    store: Store,
    global: Tables,
    spaces: Vec<Option<Private>>,
    current: Space,
    /// The current space's extent and tag, for the TLB's fast path.
    extent: u32,
    tag: u32,
    tlb: Box<[Entry; TLB_ENTRIES]>,
    /// The generation the next space made is given.
    generation: u32,
    /// The highest extent of any space: no global page may lie below it.
    highest: u32,
}

impl Mmu {
    /// An MMU with nothing mapped, at most `limit` frames in use (no more
    /// than [`MOST_FRAMES`]), and one space of extent zero, current.
    pub fn new(limit: u32) -> Result<Self, MmuError> {
        let mut tlb = Vec::new();
        tlb.try_reserve_exact(TLB_ENTRIES)
            .map_err(|_| MmuError::Exhausted)?;
        tlb.resize(TLB_ENTRIES, VACANT);
        let tlb = tlb
            .into_boxed_slice()
            .try_into()
            .map_err(|_| MmuError::Exhausted)?;
        let mut spaces = Vec::new();
        spaces.try_reserve(1).map_err(|_| MmuError::Exhausted)?;
        spaces.push(Some(Private {
            tables: Tables::new(0)?,
            extent: 0,
            generation: 0,
        }));
        let first = Space {
            index: 0,
            generation: 0,
        };
        Ok(Self {
            store: Store {
                frames: Frames {
                    slots: Vec::new(),
                    free: BitVec::new(),
                    limit: limit.min(MOST_FRAMES),
                    used: 0,
                },
                watches: OrderedMap::new(),
                pending: [BitVec::new(), BitVec::new()],
            },
            global: Tables::new(TABLES)?,
            spaces,
            current: first,
            extent: 0,
            tag: first.tag(),
            tlb,
            generation: 1,
            highest: 0,
        })
    }

    /// The most frames that may be in use.
    #[must_use]
    pub const fn limit(&self) -> u32 {
        self.store.frames.limit
    }

    /// Sets the most frames that may be in use, no more than
    /// [`MOST_FRAMES`]. Frames in use past a lower limit stay; allocation
    /// waits until fewer are.
    pub const fn set_limit(&mut self, limit: u32) {
        self.store.frames.limit = if limit < MOST_FRAMES {
            limit
        } else {
            MOST_FRAMES
        };
    }

    /// The frames in use.
    #[must_use]
    pub const fn used(&self) -> u32 {
        self.store.frames.used
    }

    /// Whether `frame` is free.
    #[must_use]
    pub fn is_free(&self, frame: Frame) -> bool {
        self.store.frames.slot(frame).is_none()
    }

    /// Takes the lowest free frame into use, zeroed and held.
    pub fn allocate(&mut self, kind: FrameKind) -> Result<Frame, MmuError> {
        let lowest = self.store.frames.lowest_free();
        let frame = Frame(u32::try_from(lowest).map_err(|_| MmuError::Exhausted)?);
        self.allocate_at(frame, kind)?;
        Ok(frame)
    }

    /// Takes `frame` into use, zeroed and held.
    pub fn allocate_at(&mut self, frame: Frame, kind: FrameKind) -> Result<(), MmuError> {
        // Refused before anything grows, so a frame past the limit costs
        // nothing.
        self.store.frames.check_take(frame)?;
        let len = self.store.frames.slots.len().max(frame.index() + 1);
        for pending in &mut self.store.pending {
            if pending.len() < len {
                pending
                    .try_resize(len, false)
                    .map_err(|_| MmuError::Exhausted)?;
            }
        }
        self.store.frames.take(frame, kind)
    }

    /// Lets go of `frame`, which is freed now if nothing maps it, or else
    /// when its last mapping goes.
    pub fn release(&mut self, frame: Frame) -> Result<(), MmuError> {
        let slot = self.store.frames.slot_mut(frame).ok_or(MmuError::Frame)?;
        if !slot.held {
            return Err(MmuError::Frame);
        }
        slot.held = false;
        if slot.maps == 0 {
            self.store.free(frame);
        }
        Ok(())
    }

    /// Holds `frame` again, so that it outlives its mappings.
    pub fn hold(&mut self, frame: Frame) -> Result<(), MmuError> {
        let slot = self.store.frames.slot_mut(frame).ok_or(MmuError::Frame)?;
        slot.held = true;
        Ok(())
    }

    /// How many page-table entries map `frame`.
    #[must_use]
    pub fn mappings(&self, frame: Frame) -> Option<u32> {
        self.store.frames.slot(frame).map(|slot| slot.maps)
    }

    /// The words of `frame`, as the host reads them.
    #[must_use]
    pub fn words(&self, frame: Frame) -> Option<&[u32; FRAME_WORDS]> {
        self.store.frames.slot(frame)?.words.as_deref()
    }

    /// The words of `frame`, for the host to write: every granule watched in
    /// it is reported, since any may change.
    pub fn words_mut(&mut self, frame: Frame) -> Option<&mut [u32; FRAME_WORDS]> {
        self.store.frames.slot(frame)?;
        self.store.report(frame, u16::MAX);
        self.store.frames.slot_mut(frame)?.words.as_deref_mut()
    }

    /// Copies `from`'s words over `to`'s; every granule watched in `to` is
    /// reported, since any may change.
    pub fn copy_frame(&mut self, from: Frame, to: Frame) -> Result<(), MmuError> {
        let frames = &self.store.frames;
        if frames.slot(from).is_none() || frames.slot(to).is_none() {
            return Err(MmuError::Frame);
        }
        if from == to {
            return Ok(());
        }
        self.store.report(to, u16::MAX);
        let slots = &mut self.store.frames.slots;
        let (source, target) = if from.index() < to.index() {
            let (low, high) = slots.split_at_mut(to.index());
            (&low[from.index()], &mut high[0])
        } else {
            let (low, high) = slots.split_at_mut(from.index());
            (&high[0], &mut low[to.index()])
        };
        match (source.words.as_deref(), target.words.as_deref_mut()) {
            (Some(source), Some(target)) => {
                *target = *source;
                Ok(())
            }
            _ => Err(MmuError::Frame),
        }
    }

    /// Moves what each watcher watches in `from` onto `to`, whose contents
    /// are `from`'s now, reporting all of it there; a report pending for
    /// `from` stays. Refused whole, moving nothing, if the host has no room
    /// for `to`'s watch.
    pub fn move_watches(&mut self, from: Frame, to: Frame) -> Result<(), MmuError> {
        let store = &mut self.store;
        store.frames.slot(to).ok_or(MmuError::Frame)?;
        let Some(watched) = store.watches.get(&from.0).map(|old| old.watched) else {
            return Ok(());
        };
        if from == to || watched.iter().all(|&granules| granules == 0) {
            return Ok(());
        }
        if store.watches.get(&to.0).is_none() {
            store
                .watches
                .try_insert(to.0, Watch::default())
                .map_err(|_| MmuError::Exhausted)?;
        }
        if let Some(new) = store.watches.get_mut(&to.0) {
            for (mask, &granules) in new.watched.iter_mut().zip(&watched) {
                *mask |= granules;
            }
        }
        if let Some(old) = store.watches.get_mut(&from.0) {
            old.watched = [0; WATCHERS];
        }
        settle(&mut store.frames, &mut store.watches, from);
        store.report(to, u16::MAX);
        Ok(())
    }

    /// The lowest free frame at or past `from`, under the limit.
    #[must_use]
    pub fn next_free(&self, from: Frame) -> Option<Frame> {
        let frames = &self.store.frames;
        let index = frames
            .free
            .next_one(from.index())
            .unwrap_or_else(|| frames.slots.len().max(from.index()));
        let number = u32::try_from(index).ok()?;
        (number < frames.limit).then_some(Frame(number))
    }

    /// Makes the tables that map `range` in `table` and keeps them until
    /// [`Mmu::unreserve`], so a mapping inside it never needs memory:
    /// refused whole if the host has none. Reservations nest.
    pub fn reserve(&mut self, table: Table, range: Range<u32>) -> Result<(), MmuError> {
        let pages = range.start >> PAGE_BITS..range.end.div_ceil(PAGE_SIZE);
        self.tables_mut(table)
            .ok_or(MmuError::Space)?
            .reserve(pages)
    }

    /// Drops a reservation [`Mmu::reserve`] made.
    pub fn unreserve(&mut self, table: Table, range: Range<u32>) {
        let pages = range.start >> PAGE_BITS..range.end.div_ceil(PAGE_SIZE);
        if let Some(tables) = self.tables_mut(table) {
            tables.release(Tables::covering(pages));
        }
    }

    /// The current space.
    #[must_use]
    pub const fn current(&self) -> Space {
        self.current
    }

    fn private(&self, space: Space) -> Option<&Private> {
        self.spaces
            .get(space.index())?
            .as_ref()
            .filter(|private| private.generation == space.generation)
    }

    fn private_mut(&mut self, space: Space) -> Option<&mut Private> {
        self.spaces
            .get_mut(space.index())?
            .as_mut()
            .filter(|private| private.generation == space.generation)
    }

    /// A space's extent.
    #[must_use]
    pub fn extent(&self, space: Space) -> Option<u32> {
        self.private(space).map(|private| private.extent)
    }

    /// Finds the highest extent again, after one fell.
    fn recount_highest(&mut self) {
        self.highest = self
            .spaces
            .iter()
            .flatten()
            .map(|private| private.extent)
            .max()
            .unwrap_or(0);
    }

    /// A new space of `extent` bytes, mapping nothing; refused where a global
    /// page lies below the extent.
    pub fn create_space(&mut self, extent: u32) -> Result<Space, MmuError> {
        if !extent.is_multiple_of(PAGE_SIZE) || self.global.any_present(0..extent >> PAGE_BITS) {
            return Err(MmuError::Placement);
        }
        let generation = self.generation;
        let private = Private {
            tables: Tables::new(tables_for(extent))?,
            extent,
            generation,
        };
        let index = if let Some(index) = self.spaces.iter().position(Option::is_none) {
            index
        } else {
            self.spaces
                .try_reserve(1)
                .map_err(|_| MmuError::Exhausted)?;
            self.spaces.push(None);
            self.spaces.len() - 1
        };
        let space = Space {
            index: u32::try_from(index).map_err(|_| MmuError::Exhausted)?,
            generation,
        };
        self.spaces[index] = Some(private);
        self.generation = generation.wrapping_add(1);
        self.highest = self.highest.max(extent);
        Ok(space)
    }

    /// Destroys a space other than the current one, unmapping its pages.
    pub fn destroy_space(&mut self, space: Space) -> Result<(), MmuError> {
        if space == self.current {
            return Err(MmuError::Space);
        }
        if self.private(space).is_none() {
            return Err(MmuError::Space);
        }
        let mut private = self
            .spaces
            .get_mut(space.index())
            .and_then(Option::take)
            .ok_or(MmuError::Space)?;
        let store = &mut self.store;
        private
            .tables
            .drain(|entry| store.unmapped(Frame(entry >> pte::FRAME_SHIFT)));
        let tag = space.tag();
        for entry in self.tlb.iter_mut().filter(|entry| entry.tag == tag) {
            *entry = VACANT;
        }
        self.recount_highest();
        Ok(())
    }

    /// Makes `space` current.
    pub fn switch(&mut self, space: Space) -> Result<(), MmuError> {
        let extent = self.extent(space).ok_or(MmuError::Space)?;
        self.current = space;
        self.extent = extent;
        self.tag = space.tag();
        Ok(())
    }

    /// Moves a space's extent: refused over a global page when it grows, and
    /// over one of its own pages when it shrinks.
    pub fn set_extent(&mut self, space: Space, extent: u32) -> Result<(), MmuError> {
        let old = self.extent(space).ok_or(MmuError::Space)?;
        if !extent.is_multiple_of(PAGE_SIZE) {
            return Err(MmuError::Placement);
        }
        if extent > old
            && self
                .global
                .any_present(old >> PAGE_BITS..extent >> PAGE_BITS)
        {
            return Err(MmuError::Placement);
        }
        let private = self.private_mut(space).ok_or(MmuError::Space)?;
        if extent < old
            && private
                .tables
                .any_present(extent >> PAGE_BITS..old >> PAGE_BITS)
        {
            return Err(MmuError::Occupied);
        }
        let count = tables_for(extent);
        let tables = &mut private.tables.tables;
        if count > tables.len() {
            tables
                .try_reserve_exact(count - tables.len())
                .map_err(|_| MmuError::Exhausted)?;
        }
        tables.resize_with(count, || None);
        private.extent = extent;
        if space == self.current {
            self.extent = extent;
        }
        if extent >= self.highest {
            self.highest = extent;
        } else if old == self.highest {
            self.recount_highest();
        }
        Ok(())
    }

    fn tables(&self, table: Table) -> Option<&Tables> {
        match table {
            Table::Global => Some(&self.global),
            Table::Space(space) => self.private(space).map(|private| &private.tables),
        }
    }

    fn tables_mut(&mut self, table: Table) -> Option<&mut Tables> {
        match table {
            Table::Global => Some(&mut self.global),
            Table::Space(space) => self.private_mut(space).map(|private| &mut private.tables),
        }
    }

    /// The page of `address` in `table`, if the table may map it there.
    fn placed(&self, table: Table, address: u32) -> Result<u32, MmuError> {
        if !address.is_multiple_of(PAGE_SIZE) {
            return Err(MmuError::Placement);
        }
        let allowed = match table {
            Table::Global => address >= self.highest,
            Table::Space(space) => address < self.extent(space).ok_or(MmuError::Space)?,
        };
        if allowed {
            Ok(address >> PAGE_BITS)
        } else {
            Err(MmuError::Placement)
        }
    }

    /// Whether `access` would let the guest write `frame`, a host frame.
    fn writes_host(&self, frame: Frame, access: Access) -> bool {
        let host = self.store.frames.slot(frame).is_some_and(|slot| slot.host);
        host && (access.user.writable() || access.privileged.writable())
    }

    /// Maps `frame` at `address` in `table` with `access`.
    pub fn map(
        &mut self,
        table: Table,
        address: u32,
        frame: Frame,
        access: Access,
    ) -> Result<(), MmuError> {
        let page = self.placed(table, address)?;
        if self.store.frames.slot(frame).is_none() {
            return Err(MmuError::Frame);
        }
        if self.writes_host(frame, access) {
            return Err(MmuError::HostWritable);
        }
        let tables = self.tables_mut(table).ok_or(MmuError::Space)?;
        if tables.entry(page) & pte::PRESENT != 0 {
            return Err(MmuError::Occupied);
        }
        tables.insert(
            page,
            (frame.0 << pte::FRAME_SHIFT) | pte::PRESENT | access.bits(),
        )?;
        if let Some(slot) = self.store.frames.slot_mut(frame) {
            slot.maps += 1;
        }
        self.invalidate(page);
        Ok(())
    }

    /// Unmaps the page at `address` in `table`, returning its frame, which is
    /// freed if this was its last mapping and nothing holds it.
    pub fn unmap(&mut self, table: Table, address: u32) -> Result<Frame, MmuError> {
        if !address.is_multiple_of(PAGE_SIZE) {
            return Err(MmuError::Placement);
        }
        let page = address >> PAGE_BITS;
        let entry = self.tables_mut(table).ok_or(MmuError::Space)?.remove(page);
        if entry & pte::PRESENT == 0 {
            return Err(MmuError::Unmapped);
        }
        let frame = Frame(entry >> pte::FRAME_SHIFT);
        self.store.unmapped(frame);
        self.invalidate(page);
        Ok(frame)
    }

    /// Changes the access of the page at `address` in `table`.
    pub fn protect(&mut self, table: Table, address: u32, access: Access) -> Result<(), MmuError> {
        if !address.is_multiple_of(PAGE_SIZE) {
            return Err(MmuError::Placement);
        }
        let page = address >> PAGE_BITS;
        let entry = self.tables(table).ok_or(MmuError::Space)?.entry(page);
        if entry & pte::PRESENT == 0 {
            return Err(MmuError::Unmapped);
        }
        if self.writes_host(Frame(entry >> pte::FRAME_SHIFT), access) {
            return Err(MmuError::HostWritable);
        }
        let replaced = (entry & !pte::ACCESS) | access.bits();
        if let Some(tables) = self.tables_mut(table) {
            tables.replace(page, replaced);
        }
        self.invalidate(page);
        Ok(())
    }

    /// The frame and access mapped at `address` in `table`.
    #[must_use]
    pub fn lookup(&self, table: Table, address: u32) -> Option<(Frame, Access)> {
        let entry = self.tables(table)?.entry(address >> PAGE_BITS);
        (entry & pte::PRESENT != 0)
            .then(|| (Frame(entry >> pte::FRAME_SHIFT), Access::from_bits(entry)))
    }

    /// The table the processor reaches `address` through now: the current
    /// space's below its extent, the global one above.
    #[must_use]
    pub const fn table_of(&self, address: u32) -> Table {
        if address < self.extent {
            Table::Space(self.current)
        } else {
            Table::Global
        }
    }

    /// Forgets the TLB's entry for `page`, whichever space it came from.
    fn invalidate(&mut self, page: u32) {
        let entry = &mut self.tlb[page as usize % TLB_ENTRIES];
        if entry.page == page {
            *entry = VACANT;
        }
    }

    /// Watches `granules` of `frame` for `watcher`: a bit each, granule zero
    /// lowest.
    pub fn watch(&mut self, frame: Frame, granules: u16, watcher: Watcher) -> Result<(), MmuError> {
        let store = &mut self.store;
        store.frames.slot(frame).ok_or(MmuError::Frame)?;
        if store.watches.get(&frame.0).is_none() {
            store
                .watches
                .try_insert(frame.0, Watch::default())
                .map_err(|_| MmuError::Exhausted)?;
        }
        let watch = store.watches.get_mut(&frame.0).ok_or(MmuError::Exhausted)?;
        watch.watched[watcher.0] |= granules;
        settle(&mut store.frames, &mut store.watches, frame);
        Ok(())
    }

    /// Stops `watcher` watching `granules` of `frame`; a report already
    /// pending stays.
    pub fn unwatch(&mut self, frame: Frame, granules: u16, watcher: Watcher) {
        let store = &mut self.store;
        let Some(watch) = store.watches.get_mut(&frame.0) else {
            return;
        };
        watch.watched[watcher.0] &= !granules;
        settle(&mut store.frames, &mut store.watches, frame);
    }

    /// Hands `watcher` each frame it has a report pending for, with the
    /// granules stored into, and watches them again.
    pub fn take_written(&mut self, watcher: Watcher, mut each: impl FnMut(Frame, u16)) {
        let Store {
            frames,
            watches,
            pending,
        } = &mut self.store;
        let set = &mut pending[watcher.0];
        let mut from = 0;
        while let Some(index) = set.next_one(from) {
            set.set(index, false);
            from = index + 1;
            let Ok(number) = u32::try_from(index) else {
                continue;
            };
            let Some(watch) = watches.get_mut(&number) else {
                continue;
            };
            let granules = core::mem::take(&mut watch.pending[watcher.0]);
            settle(frames, watches, Frame(number));
            each(Frame(number), granules);
        }
    }

    /// The TLB entry for an access of `kind` at `address`: found in the TLB,
    /// or filled from the tables, or the abort the tables answer.
    #[inline]
    fn entry(&mut self, address: u32, kind: Kind, privileged: bool) -> Result<Entry, Abort> {
        let page = address >> PAGE_BITS;
        let tag = if address < self.extent { self.tag } else { 0 };
        let want = wanted(kind, privileged);
        let entry = self.tlb[page as usize % TLB_ENTRIES];
        if entry.page == page && entry.tag == tag && entry.allows & want != 0 {
            return Ok(entry);
        }
        self.fill(page, tag, want)
    }

    #[cold]
    fn fill(&mut self, page: u32, tag: u32, want: u8) -> Result<Entry, Abort> {
        let table = if tag == 0 {
            Table::Global
        } else {
            Table::Space(self.current)
        };
        let raw = self.tables(table).map_or(0, |tables| tables.entry(page));
        if raw & pte::PRESENT == 0 {
            return Err(Abort::Translation);
        }
        let frame = Frame(raw >> pte::FRAME_SHIFT);
        let entry = Entry {
            page,
            tag,
            frame,
            allows: Access::from_bits(raw).allows(),
            host: self.store.frames.slot(frame).is_some_and(|slot| slot.host),
        };
        self.tlb[page as usize % TLB_ENTRIES] = entry;
        if entry.allows & want == 0 {
            return Err(Abort::Permission);
        }
        Ok(entry)
    }

    /// Reserves as [`Mmu::reserve`] does, each table made by `make`.
    #[cfg(test)]
    fn reserve_with(
        &mut self,
        table: Table,
        range: Range<u32>,
        make: impl FnMut() -> Option<Box<[u32; TABLE_PAGES]>>,
    ) -> Result<(), MmuError> {
        let pages = range.start >> PAGE_BITS..range.end.div_ceil(PAGE_SIZE);
        self.tables_mut(table)
            .ok_or(MmuError::Space)?
            .reserve_with(pages, make)
    }

    /// The level-2 tables `table` holds.
    #[cfg(test)]
    fn tables_held(&self, table: Table) -> usize {
        self.tables(table)
            .map_or(0, |tables| tables.tables.iter().flatten().count())
    }

    fn word(&self, frame: Frame, address: u32) -> Result<u32, Abort> {
        let words = self.words(frame).ok_or(Abort::External)?;
        Ok(words[(address as usize >> 2) % FRAME_WORDS])
    }
}

impl Bus for Mmu {
    fn fetch(&mut self, address: u32, privileged: bool) -> Result<Fetch, Abort> {
        let entry = self.entry(address, Kind::Execute, privileged)?;
        Ok(Fetch {
            word: self.word(entry.frame, address)?,
            host: entry.host,
            page: entry.frame.number(),
        })
    }

    fn read(&mut self, address: u32, width: Width, privileged: bool) -> Result<u32, Abort> {
        let entry = self.entry(address, Kind::Read, privileged)?;
        let word = self.word(entry.frame, address)?;
        let lane = word >> (8 * (address & 3));
        Ok(match width {
            Width::Word => word,
            narrow => lane & narrow.mask(),
        })
    }

    fn write(
        &mut self,
        address: u32,
        width: Width,
        value: u32,
        privileged: bool,
    ) -> Result<(), Abort> {
        let frame = self.entry(address, Kind::Write, privileged)?.frame;
        let slot = self.store.frames.slot_mut(frame).ok_or(Abort::External)?;
        let granule = 1 << ((address % PAGE_SIZE) >> GRANULE_BITS);
        let watched = slot.armed & granule != 0;
        let words = slot.words.as_deref_mut().ok_or(Abort::External)?;
        let shift = 8 * (address & 3);
        let mask = match width {
            Width::Word => u32::MAX,
            narrow => narrow.mask() << shift,
        };
        let word = &mut words[(address as usize >> 2) % FRAME_WORDS];
        *word = (*word & !mask) | ((value << shift) & mask);
        if watched {
            self.store.report(frame, granule);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
