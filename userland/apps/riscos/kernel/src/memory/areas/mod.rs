//! Dynamic areas (`plans/RISCOS-EMULATOR.md` §3): `OS_DynamicArea` whole,
//! `OS_ChangeDynamicArea` and `OS_ReadDynamicArea`, as RISC OS 5 defines
//! them, with application space and the free pool as its levers.
//!
//! The SWIs' registers are the kernel's to read and write; here each reason
//! takes and answers typed values. Where RISC OS 5 leaves a case unguarded —
//! a system area removed or renumbered over, a negative number made — the
//! manager refuses it.

mod change;
mod create;
mod pmp;
mod sparse;

use alloc::vec::Vec;
use core::ops::Range;

use tairix_collections::{OrderedMap, RangeMap};
use tairix_inline::ArrayVec;
use tairix_riscos_arm::mmu::{Access, Frame, Rights, PAGE_SIZE};

use super::layout::{COMPATIBILITY, HIGH_ZERO_PAGE, SCRATCH, ZERO_PAGE_SIZE};
use super::{Bits, GuestError, Memory};

pub use create::{Create, Created};
pub use pmp::{LogicalOp, PageQuery, PhysicalOp};

/// The free pool's number: the lever that moves memory to and from
/// application space.
pub const FREE_POOL: u32 = 6;
/// Area numbers up to this are the system's own.
const SYSTEM: u32 = 7;
/// The numbers handed out first, most recently freed first.
const QUICK: Range<u32> = 0x100..0x200;
/// The longest title kept, as RISC OS keeps them.
pub const TITLE: usize = 31;

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

/// RISC OS 5's access privileges by number, as its short-descriptor table
/// grants them: what User mode and the privileged modes may do.
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
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AccessPrivilege;

impl AccessPrivilege {
    /// The access privilege numbered `number`.
    #[must_use]
    pub const fn of(number: u32) -> Option<Access> {
        if (number as usize) < PRIVILEGES.len() {
            Some(PRIVILEGES[number as usize])
        } else {
            None
        }
    }
}

/// A dynamic area's handler: its code and the workspace it is given.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Handler {
    /// The code.
    pub code: u32,
    /// The value it is given in R12.
    pub workspace: u32,
}

/// Why a dynamic-area call failed: RISC OS's error, by number.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AreaError {
    /// "Unknown dynamic area".
    BadDynamicArea,
    /// "Can't, application running": application space holds the current
    /// active object.
    Application,
    /// "Unable to move memory".
    NotAllMoved,
    /// "Memory in use": a module refused application space's change.
    ApplicationInUse,
    /// "Dynamic area already exists".
    AreaAlreadyExists,
    /// "Area not on page boundary".
    AreaNotOnPageBoundary,
    /// "Overlapping areas".
    OverlappingAreas,
    /// "Can't allocate area".
    CantAllocateArea,
    /// "Can't allocate level 2 page tables".
    CantAllocateLevel2,
    /// "Can't get physical memory".
    CantGetPhysicalMemory,
    /// "Bad dynamic area options".
    BadDynamicAreaOptions,
    /// "Bad page number".
    BadPageNumber,
    /// "Bad address".
    BadAddress,
    /// "Bad parameters".
    BadParameters,
    /// "Illegal dynamic area renumber", RISC OS's unnumbered error.
    IllegalRenumber,
    /// "Invalid `OS_DynamicArea` sparse claim/release", unnumbered.
    InvalidSparse,
    /// The error a handler gave.
    Guest(GuestError),
}

impl AreaError {
    /// The error's number; zero for RISC OS's unnumbered ones, none for a
    /// handler's own.
    #[must_use]
    pub const fn number(self) -> Option<u32> {
        Some(match self {
            Self::BadDynamicArea => 0x105,
            Self::Application => 0x1C0,
            Self::NotAllMoved => 0x1C1,
            Self::ApplicationInUse => 0x1C2,
            Self::AreaAlreadyExists => 0x1C4,
            Self::AreaNotOnPageBoundary => 0x1C5,
            Self::OverlappingAreas => 0x1C6,
            Self::CantAllocateArea => 0x1C7,
            Self::CantAllocateLevel2 => 0x1C8,
            Self::CantGetPhysicalMemory => 0x1CA,
            Self::BadDynamicAreaOptions => 0x1D0,
            Self::BadPageNumber => 0x1D1,
            Self::BadAddress => 0xFC,
            Self::BadParameters => 0x1EA,
            Self::IllegalRenumber | Self::InvalidSparse => 0,
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
    handler: Option<Handler>,
    /// Its place in the order the areas were made.
    sequence: u64,
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

    /// The addresses it reserves.
    const fn reserved(&self) -> Range<u64> {
        let base = self.base as u64;
        let max = self.max as u64;
        if self.doubly() {
            base - max..base + max
        } else {
            base..base + max
        }
    }

    /// Where its page `index` lies, and its second copy's for a doubly
    /// mapped area.
    fn addresses(&self, index: u32) -> [Option<u32>; 2] {
        let offset = index * PAGE_SIZE;
        if self.doubly() {
            let size = self.size();
            [Some(self.base - size + offset), Some(self.base + offset)]
        } else {
            [Some(self.base + offset), None]
        }
    }

    /// The access its pages are mapped with.
    fn access(&self) -> Access {
        AccessPrivilege::of(self.flags.privilege()).unwrap_or(PRIVILEGES[2])
    }
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

/// An area's identity while it lives, whatever it is renumbered to.
pub(super) type Id = u32;

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
    /// The highest number past the quick ones given out by probing.
    treacle: u32,
    /// Shrinkable areas, the most recently made first.
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
            treacle: QUICK.end - 1,
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

    /// Records a change to `number`, as the Task Manager counts them: only
    /// areas past the system's.
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

impl Memory {
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

    /// The free pool as an area: a pool of every page not in use, its list
    /// as long as the guest's RAM.
    fn free_pool_info(&self) -> Info<'_> {
        Info {
            number: FREE_POOL,
            size: 0,
            base: 0,
            flags: AreaFlags::PMP | AreaFlags::NOT_CACHEABLE | AreaFlags::NOT_BUFFERABLE | 2,
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
        let mut index = 0;
        while let Some(&id) = self.areas.shrinkable.get(index) {
            index += 1;
            let Some(area) = self.areas.area(id) else {
                continue;
            };
            if Some(area.number) == excluding {
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
