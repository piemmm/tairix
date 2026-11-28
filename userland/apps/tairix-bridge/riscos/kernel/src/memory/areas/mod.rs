//! Dynamic areas (`plans/RISCOS-EMULATOR.md` §3): `OS_DynamicArea` whole,
//! `OS_ChangeDynamicArea` and `OS_ReadDynamicArea`, with application space
//! and the free pool as their levers.
//!
//! The SWIs' registers are the kernel's to read and write; here each reason
//! takes and answers typed values. A system area removed or renumbered over,
//! and a negative number made, are refused.
//!
//! Handlers and service calls run guest code that may call these SWIs again,
//! so whatever was checked before one runs is checked again after it: an
//! area is followed by its id and the sequence it was made at, which no area
//! made later shares.

mod change;
mod create;
mod pmp;
mod sparse;

use alloc::vec::Vec;
use core::ops::Range;

use tairix_collections::{OrderedMap, RangeMap};
use tairix_inline::ArrayVec;
use tairix_riscos_arm::mmu::{Access, Frame, Rights, GRANULE_SIZE, PAGE_SIZE};

use super::layout::{COMPATIBILITY, HIGH_ZERO_PAGE, SCRATCH, ZERO_PAGE_SIZE};
use super::{Bits, GuestError, Memory};

pub use create::{Create, Created};
pub use pmp::{LogicalOp, PageQuery, PhysicalOp};

/// The free pool's number: the lever that moves memory to and from
/// application space.
pub const FREE_POOL: u32 = 6;
/// The system heap's number.
pub const SYSTEM_HEAP: u32 = 0;
/// The RMA's number.
pub const RMA: u32 = 1;
/// The screen's number, which each context has an area of its own as.
pub const SCREEN: u32 = 2;
/// The system sprite area's number.
pub const SPRITES: u32 = 3;
/// The font cache's number.
pub const FONT_CACHE: u32 = 4;
/// The RAM disc's number.
pub const RAM_DISC: u32 = 5;
/// The free pool's flags, which its pages carry: a physical memory pool,
/// neither cacheable nor bufferable, and read-write to privileged code only.
pub const FREE_POOL_FLAGS: u32 =
    AreaFlags::PMP | AreaFlags::NOT_CACHEABLE | AreaFlags::NOT_BUFFERABLE | 2;
/// Area numbers up to this are the system's own.
pub const SYSTEM: u32 = 7;
/// The numbers handed out first, most recently freed first.
const QUICK: Range<u32> = 0x100..0x200;
/// The longest title kept; a longer one is cut.
pub const TITLE: usize = 31;
/// The most areas a session holds at once: a fixed bound on the host memory
/// a guest's area records take, far past what RISC OS software makes.
const MOST_AREAS: usize = 4096;

/// Area flags, as `OS_DynamicArea` 0 takes them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AreaFlags(pub u32);

impl AreaFlags {
    /// The access privilege's number.
    pub const PRIVILEGE: u32 = 0xF;
    /// Not bufferable.
    pub const NOT_BUFFERABLE: u32 = 1 << 4;
    /// Not cacheable.
    pub const NOT_CACHEABLE: u32 = 1 << 5;
    /// Mapped twice, its copies abutting at its base.
    pub const DOUBLY_MAPPED: u32 = 1 << 6;
    /// Not resizable by dragging in the Task Manager.
    pub const NOT_USER_DRAGGABLE: u32 = 1 << 7;
    /// Its handler may name the physical pages it grows by.
    pub const NEEDS_SPECIFIC_PAGES: u32 = 1 << 8;
    /// It may be shrunk when another area needs the memory.
    pub const SHRINKABLE: u32 = 1 << 9;
    /// Its pages are mapped where its owner claims them.
    pub const SPARSE: u32 = 1 << 10;
    /// Its pages come from memory a DMA controller can reach.
    pub const NEEDS_DMA: u32 = 1 << 15;
    /// Its handler is called on an abort in its range.
    pub const ABORTABLE: u32 = 1 << 16;
    /// A physical memory pool.
    pub const PMP: u32 = 1 << 20;
    /// The page-flag bits an area's flags give its pages.
    const PAGE_BITS: u32 = 0x0010_707F;

    const fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }

    /// The access privilege's number.
    #[must_use]
    pub const fn privilege(self) -> u32 {
        self.0 & Self::PRIVILEGE
    }
}

/// The access privileges by number, as a short-descriptor table grants them:
/// what User mode and the privileged modes may do.
const PRIVILEGES: [Access; 10] = [
    privilege(Rights::ReadWrite, Rights::ReadWrite, true),
    privilege(Rights::Read, Rights::ReadWrite, true),
    privilege(Rights::None, Rights::ReadWrite, true),
    privilege(Rights::Read, Rights::Read, true),
    privilege(Rights::None, Rights::Read, true),
    privilege(Rights::ReadWrite, Rights::ReadWrite, false),
    privilege(Rights::Read, Rights::ReadWrite, false),
    privilege(Rights::None, Rights::ReadWrite, false),
    privilege(Rights::Read, Rights::Read, false),
    privilege(Rights::None, Rights::Read, false),
];

const fn privilege(user: Rights, privileged: Rights, execute: bool) -> Access {
    Access {
        user,
        privileged,
        execute,
    }
}

/// What access privilege `number` grants, if there is one.
#[must_use]
pub const fn access_privilege(number: u32) -> Option<Access> {
    if (number as usize) < PRIVILEGES.len() {
        Some(PRIVILEGES[number as usize])
    } else {
        None
    }
}

/// The reasons a dynamic area's [`Handler`] is called with, in R0.
pub mod handler_reason {
    /// The area is to grow.
    pub const PRE_GROW: u32 = 0;
    /// The area has grown.
    pub const POST_GROW: u32 = 1;
    /// The area is to shrink.
    pub const PRE_SHRINK: u32 = 2;
    /// The area has shrunk.
    pub const POST_SHRINK: u32 = 3;
    /// How far the area could shrink.
    pub const TEST_SHRINK: u32 = 4;
    /// A physical-memory-pool area's pages to be resized.
    pub const RESIZE_PMP: u32 = 6;
}

/// A dynamic area's handler: its code and the workspace it is given.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Handler {
    /// The code.
    pub code: u32,
    /// The value it is given in R12.
    pub workspace: u32,
    /// The profile its code runs under: that of the code that made the
    /// area.
    pub bits: Bits,
}

/// Why a dynamic-area call failed: RISC OS's error, by number.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AreaError {
    /// "No such dynamic area".
    BadDynamicArea,
    /// "The running application occupies that memory": application space
    /// holds the current active object.
    Application,
    /// "Unable to move the memory asked for".
    NotAllMoved,
    /// "Memory in use": a module refused application
    /// space's change.
    ApplicationInUse,
    /// "That dynamic area exists already".
    AreaAlreadyExists,
    /// "Area base address is not page-aligned".
    AreaNotOnPageBoundary,
    /// "The area would overlap another".
    OverlappingAreas,
    /// "No logical address space left for the area".
    CantAllocateArea,
    /// "No memory for the area's page tables".
    CantAllocateLevel2,
    /// "The physical pages asked for are not free".
    CantGetPhysicalMemory,
    /// "Those dynamic area options are not allowed".
    BadDynamicAreaOptions,
    /// "Page number out of range".
    BadPageNumber,
    /// "A page asked for is reserved": a claim that may not take a page
    /// `OS_Memory` 23 reserved.
    PageReserved,
    /// "Invalid address".
    BadAddress,
    /// "Invalid parameters".
    BadParameters,
    /// "That dynamic area cannot be renumbered", RISC OS's unnumbered error.
    IllegalRenumber,
    /// "Invalid `OS_DynamicArea` sparse claim/release", unnumbered.
    InvalidSparse,
    /// The error a handler gave.
    Guest(GuestError),
}

/// "A page asked for is reserved", which `OS_Memory` 23 gives too.
pub(crate) const PAGE_RESERVED: crate::error::Error = crate::error::Error {
    number: 0x1CA,
    token: "PageReserved",
};

/// "The physical pages asked for are not free", which `OS_Memory` gives too.
pub(crate) const CANT_GET_PHYSICAL: crate::error::Error = crate::error::Error {
    number: 0x1C8,
    token: "PhysicalPagesUnavailable",
};

impl AreaError {
    /// The error's number and the token its text is found under in the
    /// kernel's message file; the number zero for RISC OS's unnumbered ones,
    /// none for a handler's own.
    #[must_use]
    pub const fn number_and_token(self) -> Option<(u32, &'static str)> {
        Some(match self {
            Self::BadDynamicArea => (0x105, "NoSuchDynamicArea"),
            Self::Application => (0x1C0, "ApplicationSpaceOccupied"),
            Self::NotAllMoved => (0x1C1, "MemoryNotMoved"),
            Self::ApplicationInUse => (0x1C2, "ApplicationSpaceInUse"),
            Self::AreaAlreadyExists => (0x1C4, "DynamicAreaExists"),
            Self::AreaNotOnPageBoundary => (0x1C5, "AreaBaseMisaligned"),
            Self::OverlappingAreas => (0x1C6, "AreasOverlap"),
            Self::CantAllocateArea => (0x1C3, "NoAddressSpaceForArea"),
            Self::CantAllocateLevel2 => (0x1C7, "NoPageTablesForArea"),
            Self::CantGetPhysicalMemory => (CANT_GET_PHYSICAL.number, CANT_GET_PHYSICAL.token),
            Self::BadDynamicAreaOptions => (0x1D0, "InvalidAreaOptions"),
            Self::BadPageNumber => (0x1D1, "PageNumberOutOfRange"),
            Self::PageReserved => (PAGE_RESERVED.number, PAGE_RESERVED.token),
            Self::BadAddress => (
                crate::error::BAD_ADDRESS.number,
                crate::error::BAD_ADDRESS.token,
            ),
            Self::BadParameters => (
                crate::error::BAD_PARAMETERS.number,
                crate::error::BAD_PARAMETERS.token,
            ),
            Self::IllegalRenumber => (0, "AreaRenumberRefused"),
            Self::InvalidSparse => (0, "BadSparseOperation"),
            Self::Guest(_) => return None,
        })
    }

    /// The error a handler's refusal makes: its own, or "Unable to move
    /// memory" when it named none.
    const fn refused(error: Option<GuestError>) -> Self {
        match error {
            Some(error) => Self::Guest(error),
            None => Self::NotAllMoved,
        }
    }
}

/// A change that moved part of what was asked, or nothing, and why.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Failure {
    /// The error.
    pub error: AreaError,
    /// The bytes that moved before it.
    pub moved: u32,
}

impl Failure {
    const fn none(error: AreaError) -> Self {
        Self { error, moved: 0 }
    }
}

/// The three clamps on areas' maximum sizes (`OS_DynamicArea` 8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Clamps {
    /// The most an area made without a maximum may reach.
    pub unbounded: u32,
    /// The most an area made with one may.
    pub bounded: u32,
    /// The most a sparse area may.
    pub sparse: u32,
}

/// A clamp below this is no clamp: `OS_DynamicArea` 8 leaves it be.
const CLAMP_FLOOR: u32 = 0x0010_0000;

/// A title, as RISC OS keeps it: bytes, not text.
type Title = ArrayVec<u8, TITLE>;

/// The free pool's place in the order by title: made before every other
/// area, so after any of the same title.
const POOL_KEY: ([u8; TITLE], u64) = (folded(b"Free pool"), u64::MAX);

/// How an area's pages are laid out.
enum Shape {
    /// From the base up, page by page; for a doubly mapped area, both
    /// copies.
    Pages(Vec<Frame>),
    /// Wherever its owner claimed them, by page index from the base.
    Sparse {
        pages: OrderedMap<u32, Frame>,
        high_water: u32,
    },
    /// A physical memory pool: its page list, and which of its pages are
    /// mapped where in its window.
    Pmp(pmp::Pool),
}

/// A dynamic area.
pub struct Area {
    number: u32,
    title: Title,
    /// The base as RISC OS reports it: for a doubly mapped area, where its
    /// copies meet.
    base: u32,
    max: u32,
    flags: AreaFlags,
    /// The access its pages are mapped with, from its flags' privilege.
    access: Access,
    handler: Option<Handler>,
    /// Its place in the order the areas were made: no two areas share one.
    sequence: u64,
    /// The addresses it reserves, as reserved.
    reserved: Range<u64>,
    shape: Shape,
}

impl Area {
    /// The bytes it maps.
    fn size(&self) -> u32 {
        let pages = match &self.shape {
            Shape::Pages(pages) => pages.len(),
            Shape::Sparse { pages, .. } => pages.len(),
            Shape::Pmp(pool) => pool.mapped(),
        };
        u32::try_from(pages).unwrap_or(0) * PAGE_SIZE
    }

    const fn doubly(&self) -> bool {
        self.flags.has(AreaFlags::DOUBLY_MAPPED)
    }

    /// Where its page `index` lies, and its first copy's for a doubly mapped
    /// area.
    fn addresses(&self, index: u32) -> (u32, Option<u32>) {
        let upper = self.base + index * PAGE_SIZE;
        if self.doubly() {
            let count = self.size() / PAGE_SIZE;
            (upper, Some(lower_copy(self.base, count, index)))
        } else {
            (upper, None)
        }
    }
}

/// Where page `index` of a doubly mapped area based at `base` and `count`
/// pages long has its first copy: below the base by the area's size.
const fn lower_copy(base: u32, count: u32, index: u32) -> u32 {
    base - count * PAGE_SIZE + index * PAGE_SIZE
}

/// An area's details (`OS_DynamicArea` 2, 7, 24 and 28).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Info<'a> {
    /// Its number.
    pub number: u32,
    /// The bytes mapped.
    pub size: u32,
    /// Its base as RISC OS reports it.
    pub base: u32,
    /// Its flags as made.
    pub flags: u32,
    /// Its logical maximum.
    pub max: u32,
    /// Its handler.
    pub handler: Option<Handler>,
    /// Its title.
    pub title: &'a [u8],
    /// For a pool, its pages and the most its page list holds.
    pub pool: Option<(u32, u32)>,
}

/// What the areas a session's contexts share hold, in bytes, as the Task
/// Manager shows them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Shared {
    /// The system heap.
    pub system_heap: u32,
    /// The relocatable module area.
    pub rma: u32,
    /// The system sprite area.
    pub sprites: u32,
    /// The font cache.
    pub font_cache: u32,
    /// The RAM disc.
    pub ram_disc: u32,
    /// Every area a program made.
    pub made: u32,
    /// The free pool.
    pub free: u32,
}

/// What the current context holds of its own, in bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Own {
    /// Its application slot.
    pub slot: u32,
    /// Its screen.
    pub screen: u32,
}

/// What `OS_ReadDynamicArea` answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Read {
    /// Its start: for a doubly mapped area, its first copy's.
    pub start: u32,
    /// Its size, in bytes; a pool's is its pages'.
    pub size: u64,
    /// Its maximum, in bytes; a pool's is its page list's.
    pub max: u64,
}

/// What `OS_DynamicArea` 20 found at an address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Located {
    /// One of `OS_Memory` 16's regions, by number.
    Region(u32),
    /// A dynamic area's reservation.
    Area(u32),
}

/// Which areas changed since the Task Manager last asked
/// (`OS_DynamicArea` 6).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Changes {
    signature: u32,
    /// The area, while exactly one change has happened.
    only: Option<u32>,
    count: u32,
}

mod signature {
    pub const CREATED: u32 = 1 << 0;
    pub const REMOVED: u32 = 1 << 1;
    pub const RESIZED: u32 = 1 << 2;
    pub const RENUMBERED: u32 = 1 << 3;
}

/// An area's identity while it lives, whatever it is renumbered to; another
/// area may have it once it is gone.
pub(super) type Id = u32;

/// An area as an operation follows it across guest code: its id, and the
/// sequence it was made at, which no other area shares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Key {
    id: Id,
    sequence: u64,
}

/// Every dynamic area.
pub(super) struct Areas {
    /// The areas, by id.
    entries: Vec<Option<Area>>,
    /// Ids free for the next areas.
    free_ids: Vec<Id>,
    /// The areas by number.
    numbers: OrderedMap<u32, Id>,
    /// The areas by title, folded for a case-blind order, a later area
    /// before an earlier of the same title.
    names: OrderedMap<([u8; TITLE], u64), Id>,
    /// The addresses each area reserves, and the kernel's own parts.
    reserved: RangeMap<u64, Id>,
    /// Quick numbers free, the next to give out last.
    quick: Vec<u32>,
    /// Where probing for a number past the quick ones starts, less one: one
    /// further for each area made with a number past them.
    probe_from: u32,
    /// Shrinkable areas, in the order they were made.
    shrinkable: Vec<Id>,
    clamps: Clamps,
    changes: Changes,
    sequence: u64,
    /// A resize in progress, which no nested resize may interleave with.
    resizing: bool,
}

impl Areas {
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::new(),
            free_ids: Vec::new(),
            numbers: OrderedMap::new(),
            names: OrderedMap::new(),
            reserved: RangeMap::new(),
            quick: (QUICK.start..QUICK.end).rev().collect(),
            probe_from: QUICK.end - 1,
            shrinkable: Vec::new(),
            clamps: Clamps {
                unbounded: u32::MAX,
                bounded: u32::MAX,
                sparse: 0xFFFF_F000,
            },
            changes: Changes::default(),
            sequence: 0,
            resizing: false,
        }
    }

    /// The lowest address any area reserves in the 26-bit window: where a
    /// 32-bit slot growing into it must stop.
    pub(super) fn window_floor(&self) -> Option<u32> {
        self.lowest_reservation_above(0)
    }

    /// The lowest start of any area's reservation at or above `address`.
    pub(super) fn lowest_reservation_above(&self, address: u32) -> Option<u32> {
        self.reserved
            .overlapping(u64::from(address)..1 << 32)
            .next()
            .and_then(|(range, _)| u32::try_from(range.start).ok())
    }

    fn id(&self, number: u32) -> Result<Id, AreaError> {
        self.numbers
            .get(&number)
            .copied()
            .ok_or(AreaError::BadDynamicArea)
    }

    fn get(&self, number: u32) -> Result<&Area, AreaError> {
        self.area(self.id(number)?).ok_or(AreaError::BadDynamicArea)
    }

    fn area(&self, id: Id) -> Option<&Area> {
        self.entries.get(id as usize)?.as_ref()
    }

    fn area_mut(&mut self, id: Id) -> Option<&mut Area> {
        self.entries.get_mut(id as usize)?.as_mut()
    }

    /// Area `id`'s key, if it lives.
    fn key(&self, id: Id) -> Option<Key> {
        self.area(id).map(|area| Key {
            id,
            sequence: area.sequence,
        })
    }

    /// The area `key` follows, if it still lives.
    fn area_at(&self, key: Key) -> Option<&Area> {
        self.area(key.id)
            .filter(|area| area.sequence == key.sequence)
    }

    fn area_at_mut(&mut self, key: Key) -> Option<&mut Area> {
        self.area_mut(key.id)
            .filter(|area| area.sequence == key.sequence)
    }

    /// The shrinkable areas as they stand, the most recently made first,
    /// each with its sequence: a walk that runs guest code between steps
    /// visits these and no others, so it ends.
    fn shrinkable_now(&self) -> Vec<Key> {
        let mut now = Vec::new();
        if now.try_reserve_exact(self.shrinkable.len()).is_err() {
            return now;
        }
        now.extend(self.shrinkable.iter().rev().filter_map(|&id| self.key(id)));
        now
    }

    /// The areas live now.
    fn count(&self) -> usize {
        self.numbers.len()
    }

    /// Records a change to `number`: only areas past the system's are
    /// counted.
    fn note(&mut self, number: u32, what: u32) {
        if number <= SYSTEM {
            return;
        }
        let changes = &mut self.changes;
        changes.signature |= what;
        changes.count = changes.count.saturating_add(1);
        changes.only = (changes.count == 1).then_some(number);
    }
}

/// A title folded for the alphabetical order: case-blind, as the UK
/// territory sorts ASCII.
const fn folded(title: &[u8]) -> [u8; TITLE] {
    let mut key = [0; TITLE];
    let mut i = 0;
    while i < TITLE && i < title.len() {
        key[i] = title[i].to_ascii_lowercase();
        i += 1;
    }
    key
}

/// Where an area made `sequence`th with `title` stands in the order by
/// title: a later one before an earlier of the same title.
const fn name_key(title: &[u8], sequence: u64) -> ([u8; TITLE], u64) {
    (folded(title), u64::MAX - sequence)
}

/// A dynamic area [`Memory::park_area`] set aside.
#[derive(Debug, PartialEq, Eq)]
pub struct ParkedArea {
    key: Key,
    shrinkable: bool,
}

/// The watcher that tells a display which of an area's bytes changed: the
/// first of the MMU's, the translator's another.
const DISPLAY: Option<tairix_riscos_arm::mmu::Watcher> = tairix_riscos_arm::mmu::Watcher::new(0);

impl Memory {
    /// Watches each page area `number` holds for the display, so a store
    /// into any is reported. Pages the area gains are watched when this is
    /// asked again.
    ///
    /// # Errors
    ///
    /// An area that is not one of pages of its own, or an MMU with no room
    /// to record the watch.
    pub fn watch_area_for_display(&mut self, number: u32) -> Result<(), AreaError> {
        let watcher = DISPLAY.ok_or(AreaError::BadDynamicArea)?;
        let area = self.areas.get(number)?;
        let Shape::Pages(list) = &area.shape else {
            return Err(AreaError::BadDynamicArea);
        };
        for &frame in list {
            self.mmu
                .watch(frame, u16::MAX, watcher)
                .map_err(|_| AreaError::CantAllocateArea)?;
        }
        Ok(())
    }

    /// Hands `each` the offset into area `number` of every granule of it
    /// stored into since this last asked, the lowest first, and watches them
    /// again. Where there is no room to sort the stores by page, every
    /// granule of the area is given.
    pub fn take_display_damage(&mut self, number: u32, mut each: impl FnMut(u32)) {
        let Some(watcher) = DISPLAY else {
            return;
        };
        let mut written: Vec<(Frame, u16)> = Vec::new();
        let mut whole = true;
        self.mmu.take_written(watcher, |frame, granules| {
            whole &= tairix_util::fallible::push(&mut written, (frame, granules));
        });
        let Ok(area) = self.areas.get(number) else {
            return;
        };
        let Shape::Pages(list) = &area.shape else {
            return;
        };
        written.sort_unstable_by_key(|(frame, _)| frame.number());
        for (page, frame) in (0u32..).zip(list) {
            let granules = if whole {
                match written.binary_search_by_key(&frame.number(), |(f, _)| f.number()) {
                    Ok(found) => written[found].1,
                    Err(_) => continue,
                }
            } else {
                u16::MAX
            };
            for granule in 0..PAGE_SIZE / GRANULE_SIZE {
                if granules & (1 << granule) != 0 {
                    each(page * PAGE_SIZE + granule * GRANULE_SIZE);
                }
            }
        }
    }

    /// Area `number` set aside, its pages and the addresses it reserves
    /// kept: no number, title or enumeration finds it, nothing shrinks it,
    /// and another may take its number until [`Self::unpark_area`] gives it
    /// back.
    ///
    /// # Errors
    ///
    /// No such area.
    pub fn park_area(&mut self, number: u32) -> Result<ParkedArea, AreaError> {
        let areas = &mut self.areas;
        let id = areas.id(number)?;
        let area = areas.area(id).ok_or(AreaError::BadDynamicArea)?;
        let key = Key {
            id,
            sequence: area.sequence,
        };
        let name = name_key(area.title.as_slice(), area.sequence);
        areas.numbers.remove(&number);
        areas.names.remove(&name);
        let shrinkable = areas.shrinkable.contains(&id);
        areas.shrinkable.retain(|&other| other != id);
        Ok(ParkedArea { key, shrinkable })
    }

    /// `parked` given back its number and title, and shrunk again where it
    /// was shrinkable.
    ///
    /// # Errors
    ///
    /// "That dynamic area exists already" where its number was taken
    /// meanwhile, or no room to record it; it stays parked either way.
    pub fn unpark_area(&mut self, parked: &ParkedArea) -> Result<(), AreaError> {
        let areas = &mut self.areas;
        let area = areas.area_at(parked.key).ok_or(AreaError::BadDynamicArea)?;
        let (number, name) = (area.number, name_key(area.title.as_slice(), area.sequence));
        if areas.numbers.contains_key(&number) {
            return Err(AreaError::AreaAlreadyExists);
        }
        let id = parked.key.id;
        if parked.shrinkable && areas.shrinkable.try_reserve(1).is_err() {
            return Err(AreaError::CantAllocateArea);
        }
        areas
            .numbers
            .try_insert(number, id)
            .map_err(|_| AreaError::CantAllocateArea)?;
        if areas.names.try_insert(name, id).is_err() {
            areas.numbers.remove(&number);
            return Err(AreaError::CantAllocateArea);
        }
        if parked.shrinkable {
            areas.shrinkable.push(id);
        }
        Ok(())
    }

    /// `OS_DynamicArea` 2 and 24: an area's details.
    pub fn area_info(&self, number: u32) -> Result<Info<'_>, AreaError> {
        if number == FREE_POOL {
            return Ok(self.free_pool_info());
        }
        let area = self.areas.get(number)?;
        Ok(Info {
            number,
            size: area.size(),
            base: area.base,
            flags: area.flags.0,
            max: area.max,
            handler: area.handler,
            title: area.title.as_slice(),
            pool: match &area.shape {
                Shape::Pmp(pool) => Some((pool.pages(), pool.capacity())),
                _ => None,
            },
        })
    }

    /// What the current context holds of its own.
    #[must_use]
    pub fn own(&self) -> Own {
        Own {
            slot: self
                .current_slot()
                .and_then(|slot| self.slot_size(slot))
                .unwrap_or(0),
            screen: self.area_info(SCREEN).map_or(0, |info| info.size),
        }
    }

    /// What the areas every context shares hold, and the free pool.
    #[must_use]
    pub fn shared(&self) -> Shared {
        let size = |number| self.area_info(number).map_or(0, |info| info.size);
        let made = self
            .areas
            .names
            .values()
            .filter_map(|&id| self.areas.area(id))
            .filter(|area| area.number > SYSTEM)
            .fold(0u32, |sum, area| sum.saturating_add(area.size()));
        Shared {
            system_heap: size(SYSTEM_HEAP),
            rma: size(RMA),
            sprites: size(SPRITES),
            font_cache: size(FONT_CACHE),
            ram_disc: size(RAM_DISC),
            made,
            free: self.free_pages().saturating_mul(PAGE_SIZE),
        }
    }

    /// The free pool as an area: a pool of every page not in use, its list
    /// as long as the guest's RAM.
    fn free_pool_info(&self) -> Info<'_> {
        Info {
            number: FREE_POOL,
            size: 0,
            base: 0,
            flags: FREE_POOL_FLAGS,
            max: 0,
            handler: None,
            title: b"Free pool",
            pool: Some((self.free_pages(), self.ceiling.pages())),
        }
    }

    /// `OS_ReadDynamicArea`: an area's start, size and maximum, application
    /// space's for -1.
    pub fn read_area(&self, number: i32) -> Result<Read, AreaError> {
        if number == -1 {
            let slot = self.current_slot();
            let size = slot.and_then(|slot| self.slot_size(slot)).unwrap_or(0);
            let max = slot.and_then(|slot| self.slot_limit(slot)).unwrap_or(0);
            return Ok(Read {
                start: super::layout::APPLICATION,
                size: u64::from(size),
                max: u64::from(max),
            });
        }
        let number = u32::try_from(number).map_err(|_| AreaError::BadDynamicArea)?;
        let info = self.area_info(number)?;
        Ok(match info.pool {
            Some((pages, capacity)) => Read {
                start: info.base,
                size: u64::from(pages) * u64::from(PAGE_SIZE),
                max: u64::from(capacity) * u64::from(PAGE_SIZE),
            },
            None => Read {
                start: if AreaFlags(info.flags).has(AreaFlags::DOUBLY_MAPPED) {
                    info.base - info.size
                } else {
                    info.base
                },
                size: u64::from(info.size),
                max: u64::from(info.max),
            },
        })
    }

    /// `OS_DynamicArea` 3: the area after `after` by title, or the first;
    /// none past the last.
    pub fn enumerate_areas(&self, after: Option<u32>) -> Result<Option<u32>, AreaError> {
        use core::ops::Bound::{Excluded, Unbounded};
        let from = match after {
            None => None,
            Some(FREE_POOL) => Some(POOL_KEY),
            Some(number) => {
                let area = self.areas.get(number)?;
                Some(name_key(area.title.as_slice(), area.sequence))
            }
        };
        let next = match from {
            None => self.areas.names.iter().next(),
            Some(key) => self.areas.names.range((Excluded(key), Unbounded)).next(),
        };
        let pool_next = from.is_none_or(|key| POOL_KEY > key);
        Ok(match next {
            Some((&key, _)) if pool_next && POOL_KEY < key => Some(FREE_POOL),
            Some((_, &id)) => self.areas.area(id).map(|area| area.number),
            None => pool_next.then_some(FREE_POOL),
        })
    }

    /// `OS_DynamicArea` 7 and 28: the next area past the system's, by title.
    pub fn enumerate_area_info(&self, after: Option<u32>) -> Result<Option<Info<'_>>, AreaError> {
        let mut after = after;
        loop {
            match self.enumerate_areas(after)? {
                None => return Ok(None),
                Some(number) if number > SYSTEM => return self.area_info(number).map(Some),
                Some(number) => after = Some(number),
            }
        }
    }

    /// `OS_DynamicArea` 6: the area changed, if exactly one change happened
    /// since the last call, and what changed; both are then cleared.
    pub fn take_area_changes(&mut self) -> (Option<u32>, u32) {
        let changes = core::mem::take(&mut self.areas.changes);
        (changes.only, changes.signature)
    }

    /// `OS_DynamicArea` 8: sets the clamps a value at or past 1 MiB names,
    /// returning those before.
    pub fn set_clamps(&mut self, clamps: Clamps) -> Clamps {
        let old = self.areas.clamps;
        let set = |new: u32, old: u32| if new >= CLAMP_FLOOR { new } else { old };
        self.areas.clamps = Clamps {
            unbounded: set(clamps.unbounded, old.unbounded),
            bounded: set(clamps.bounded, old.bounded),
            sparse: set(clamps.sparse, old.sparse),
        };
        old
    }

    /// `OS_Memory` 16's regions as the current task sees them, each by
    /// number; a region a task does not see is empty. Zero page is region
    /// 12 at &FFFF0000, and at zero too for a 26-bit task.
    #[must_use]
    pub fn regions(&self) -> [(u32, Range<u32>); 8] {
        let stacks = &self.layout.stacks;
        let bits = self.current_slot().and_then(|slot| self.slot_bits(slot));
        let (zero_page, compatibility) = match bits {
            Some(Bits::Bit26) => (0..ZERO_PAGE_SIZE, 0..0),
            _ => (0..0, COMPATIBILITY),
        };
        [
            (2, stacks.irq.clone()),
            (3, stacks.supervisor.clone()),
            (4, stacks.abort.clone()),
            (5, stacks.undefined.clone()),
            (12, HIGH_ZERO_PAGE..HIGH_ZERO_PAGE + ZERO_PAGE_SIZE),
            (12, zero_page),
            (15, SCRATCH),
            (16, compatibility),
        ]
    }

    /// `OS_DynamicArea` 20: the region or area reserving `address`.
    pub fn locate(&self, address: u32) -> Result<Located, AreaError> {
        if let Some((region, _)) = self
            .regions()
            .into_iter()
            .find(|(_, range)| range.contains(&address))
        {
            return Ok(Located::Region(region));
        }
        self.areas
            .reserved
            .covering(u64::from(address))
            .and_then(|(_, &id)| self.areas.area(id))
            .map(|area| Located::Area(area.number))
            .ok_or(AreaError::BadAddress)
    }

    /// `OS_DynamicArea` 5 and 27: the pages an area could be given, the
    /// free pool's and what the shrinkable areas but `excluding` would give
    /// up; application space is not counted.
    pub fn free_area_pages<G: super::Guest + ?Sized>(
        &mut self,
        excluding: Option<u32>,
        guest: &mut G,
    ) -> Result<u32, AreaError> {
        if let Some(excluded) = excluding {
            if excluded != FREE_POOL {
                self.areas.get(excluded)?;
            }
        }
        let mut pages = self.free_pages();
        for key in self.areas.shrinkable_now() {
            let Some(area) = self.areas.area_at(key) else {
                continue;
            };
            if Some(area.number) == excluding || !matches!(area.shape, Shape::Pages(_)) {
                continue;
            }
            let (size, handler) = (area.size(), area.handler);
            // With no handler to ask, all of it may go.
            let bytes = match handler {
                Some(handler) => guest.test_shrink(self, handler, size).min(size),
                None => size,
            };
            pages = pages.saturating_add(bytes / PAGE_SIZE);
        }
        Ok(pages)
    }

    /// Where page `index` of area `id` is mapped — a pool's slot `index` —
    /// and the page flags the area gives it; none for a pool page not
    /// mapped.
    pub(super) fn area_page_place(&self, id: Id, index: u32) -> Option<(u32, u32)> {
        let area = self.areas.area(id)?;
        let flags = area.flags.0 & AreaFlags::PAGE_BITS;
        let logical = match &area.shape {
            Shape::Pages(_) => area.addresses(index).0,
            Shape::Sparse { .. } => area.base + index * PAGE_SIZE,
            Shape::Pmp(pool) => area.base + pool.window_of(index)? * PAGE_SIZE,
        };
        Some((logical, flags))
    }

    /// Where the run of pages an area maps alike from `address` ends: the
    /// end of its mapped part for an area laid out from its base, none for
    /// one whose pages lie where they were claimed.
    pub(super) fn area_run(&self, address: u32) -> Option<u32> {
        let (_, &id) = self.areas.reserved.covering(u64::from(address))?;
        let area = self.areas.area(id)?;
        let Shape::Pages(_) = area.shape else {
            return None;
        };
        let size = area.size();
        let (start, end) = if area.doubly() {
            (area.base - size, area.base + size)
        } else {
            (area.base, area.base + size)
        };
        (start..end).contains(&address).then_some(end)
    }

    /// Whether `address` lies in an abortable area: the address of its
    /// handler when it does.
    #[must_use]
    pub fn abortable_at(&self, address: u32) -> Option<(u32, Handler)> {
        let (_, &id) = self.areas.reserved.covering(u64::from(address))?;
        let area = self.areas.area(id)?;
        if !area.flags.has(AreaFlags::ABORTABLE) {
            return None;
        }
        area.handler.map(|handler| (area.number, handler))
    }
}

#[cfg(test)]
mod tests;
