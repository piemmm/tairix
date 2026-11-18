//! Task slots (`plans/RISCOS-EMULATOR.md` §3): each task's application
//! space, from &8000, in a space of its own, so a task switch swaps one
//! pointer whatever the slot holds.
//!
//! A 26-bit task's slot stops at 28 MiB. A 32-bit task's may grow into the
//! window's free space, but only while no 26-bit code is resident; a growth
//! that rule refuses says so.

use alloc::vec::Vec;

use tairix_riscos_arm::mmu::{Access, Frame, Rights, Space, Table, PAGE_SIZE, TABLE_SPAN};

use super::layout::APPLICATION;
use super::{Holder, Memory, MemoryError};

/// Application space: everyone's, everything.
const APPLICATION_ACCESS: Access = Access {
    user: Rights::ReadWrite,
    privileged: Rights::ReadWrite,
    execute: true,
};

/// The top of an application space holding `bytes`, rounded up to whole
/// pages, if it fits the address space.
fn top_of(bytes: u32) -> Option<u32> {
    bytes
        .div_ceil(PAGE_SIZE)
        .checked_mul(PAGE_SIZE)
        .and_then(|size| size.checked_add(APPLICATION))
}

/// The addressing a task's program uses.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Bits {
    /// 26-bit: the combined PC and PSR, everything below 64 MiB.
    Bit26,
    /// 32-bit.
    Bit32,
}

/// A task's slot. A handle outliving its slot names nothing.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Slot {
    index: u32,
    generation: u32,
}

struct State {
    space: Space,
    bits: Bits,
    generation: u32,
    /// Application space's pages, from &8000 up.
    pages: Vec<Frame>,
    /// The tables reserved for it, from the first: enough for its top.
    tables: u32,
}

impl State {
    fn top(&self) -> u32 {
        APPLICATION + u32::try_from(self.pages.len()).unwrap_or(0) * PAGE_SIZE
    }
}

/// Every slot.
pub(super) struct Slots {
    entries: Vec<Option<State>>,
    current: Option<Slot>,
    generation: u32,
    /// The application-space limit `OS_DynamicArea` 26 sets: no slot's top
    /// passes it. None until first set: the layout's own.
    limit: Option<u32>,
    /// The slots of 26-bit programs.
    bit26: u32,
}

impl Slots {
    pub(super) const fn new() -> Self {
        Self {
            entries: Vec::new(),
            current: None,
            generation: 0,
            limit: None,
            bit26: 0,
        }
    }

    fn state(&self, slot: Slot) -> Option<&State> {
        self.entries
            .get(slot.index as usize)?
            .as_ref()
            .filter(|state| state.generation == slot.generation)
    }

    fn state_mut(&mut self, slot: Slot) -> Option<&mut State> {
        self.entries
            .get_mut(slot.index as usize)?
            .as_mut()
            .filter(|state| state.generation == slot.generation)
    }

    /// Whether any slot's program is 26-bit.
    pub(super) const fn any_26bit(&self) -> bool {
        self.bit26 > 0
    }

    /// The spaces of the slots whose programs are of `bits`.
    pub(super) fn spaces(&self, bits: Bits) -> Result<Vec<Space>, MemoryError> {
        let mut spaces = Vec::new();
        spaces.try_reserve_exact(self.entries.len())?;
        spaces.extend(
            self.entries
                .iter()
                .flatten()
                .filter(|state| state.bits == bits)
                .map(|state| state.space),
        );
        Ok(spaces)
    }

    /// The highest top of any slot.
    pub(super) fn highest_top(&self) -> u32 {
        self.entries
            .iter()
            .flatten()
            .map(State::top)
            .max()
            .unwrap_or(APPLICATION)
    }

    /// The slot holding page `index` of slot number `slot`, with its space.
    pub(super) fn page(&self, slot: u32, index: u32) -> Option<(Space, Frame)> {
        let state = self.entries.get(slot as usize)?.as_ref()?;
        Some((state.space, *state.pages.get(index as usize)?))
    }

    /// Replaces page `index` of slot number `slot` with `frame`.
    pub(super) fn replace(&mut self, slot: u32, index: u32, frame: Frame) {
        if let Some(Some(state)) = self.entries.get_mut(slot as usize) {
            if let Some(page) = state.pages.get_mut(index as usize) {
                *page = frame;
            }
        }
    }
}

impl Memory {
    /// A new slot for a program of `bits`, empty, with the low pages that
    /// program sees.
    pub fn create_slot(&mut self, bits: Bits) -> Result<Slot, MemoryError> {
        if bits == Bits::Bit26 && !self.layout.admits_26bit {
            return Err(MemoryError::Not26Bit);
        }
        self.slots.entries.try_reserve(1)?;
        let entries = &self.slots.entries;
        let index = entries
            .iter()
            .position(Option::is_none)
            .unwrap_or(entries.len());
        let number = u32::try_from(index).map_err(|_| MemoryError::NoMemory)?;
        let space = self.mmu.create_space(APPLICATION)?;
        if let Err(error) = self.map_low(Table::Space(space), bits) {
            let _ = self.mmu.destroy_space(space);
            return Err(error);
        }
        let slots = &mut self.slots;
        let generation = slots.generation;
        slots.generation = generation.wrapping_add(1);
        let state = Some(State {
            space,
            bits,
            generation,
            pages: Vec::new(),
            tables: 0,
        });
        match slots.entries.get_mut(index) {
            Some(entry) => *entry = state,
            None => slots.entries.push(state),
        }
        if bits == Bits::Bit26 {
            slots.bit26 += 1;
        }
        Ok(Slot {
            index: number,
            generation,
        })
    }

    /// Destroys a slot that is not current, freeing its application space.
    pub fn destroy_slot(&mut self, slot: Slot) -> Result<(), MemoryError> {
        if self.slots.current == Some(slot) {
            return Err(MemoryError::Slot);
        }
        let space = self.slots.state(slot).ok_or(MemoryError::Slot)?.space;
        // The space goes first: refused, the slot is as it was.
        self.mmu.destroy_space(space)?;
        let state = self
            .slots
            .entries
            .get_mut(slot.index as usize)
            .and_then(Option::take)
            .ok_or(MemoryError::Slot)?;
        for &frame in &state.pages {
            self.pages.free(frame);
        }
        if state.bits == Bits::Bit26 {
            self.slots.bit26 -= 1;
        }
        Ok(())
    }

    /// Makes `slot` the current task's.
    pub fn switch_slot(&mut self, slot: Slot) -> Result<(), MemoryError> {
        let space = self.slots.state(slot).ok_or(MemoryError::Slot)?.space;
        self.mmu.switch(space)?;
        self.slots.current = Some(slot);
        Ok(())
    }

    /// The current task's slot.
    #[must_use]
    pub const fn current_slot(&self) -> Option<Slot> {
        self.slots.current
    }

    /// The program's addressing a slot was made for.
    #[must_use]
    pub fn slot_bits(&self, slot: Slot) -> Option<Bits> {
        self.slots.state(slot).map(|state| state.bits)
    }

    /// A slot's size: its application space above &8000.
    #[must_use]
    pub fn slot_size(&self, slot: Slot) -> Option<u32> {
        self.slots
            .state(slot)
            .map(|state| state.top() - APPLICATION)
    }

    /// The application-space limit: no slot's top passes it.
    #[must_use]
    pub fn application_limit(&self) -> u32 {
        self.slots.limit.unwrap_or(self.layout.top_32bit)
    }

    /// The highest a slot of `bits` may reach now, and whether the 26-bit
    /// window is what holds it there.
    fn slot_ceiling(&self, bits: Bits) -> (u32, bool) {
        let limit = self.application_limit();
        match bits {
            Bits::Bit26 => (limit.min(self.layout.top_26bit), false),
            Bits::Bit32 => {
                let floor = self.areas.window_floor().unwrap_or(self.layout.top_32bit);
                let open = limit.min(floor);
                if self.layout.window.is_some()
                    && self.resident_26bit()
                    && open > self.layout.top_26bit
                {
                    (self.layout.top_26bit, true)
                } else {
                    (open, false)
                }
            }
        }
    }

    /// The most a slot's application space may hold now: what its ceiling
    /// and the free pool allow.
    #[must_use]
    pub fn slot_limit(&self, slot: Slot) -> Option<u32> {
        let state = self.slots.state(slot)?;
        let (ceiling, _) = self.slot_ceiling(state.bits);
        let free = u64::from(self.free_pages()) * u64::from(PAGE_SIZE);
        let reachable = (u64::from(state.top()) + free).min(u64::from(ceiling));
        Some(
            u32::try_from(reachable)
                .unwrap_or(ceiling)
                .saturating_sub(APPLICATION),
        )
    }

    /// Whether a slot's application space may reach `bytes` under its
    /// ceiling, the free pool aside.
    pub(super) fn slot_may_reach(&self, slot: Slot, bytes: u32) -> bool {
        let Some(state) = self.slots.state(slot) else {
            return false;
        };
        let (ceiling, _) = self.slot_ceiling(state.bits);
        top_of(bytes).is_some_and(|top| top <= ceiling)
    }

    /// Sets a slot's size, rounded up to whole pages, wholly or not at all.
    pub fn set_slot_size(&mut self, slot: Slot, bytes: u32) -> Result<(), MemoryError> {
        let state = self.slots.state(slot).ok_or(MemoryError::Slot)?;
        let top = top_of(bytes).ok_or(MemoryError::TooLarge)?;
        let (bits, old, space) = (state.bits, state.top(), state.space);
        if top < old {
            self.shrink_slot(slot, (old - top) / PAGE_SIZE);
            return Ok(());
        }
        let (ceiling, window) = self.slot_ceiling(bits);
        if top > ceiling {
            return Err(if window {
                MemoryError::Window
            } else {
                MemoryError::TooLarge
            });
        }
        let added = (top - old) / PAGE_SIZE;
        if added > self.free_pages() {
            return Err(MemoryError::NoMemory);
        }
        self.slots
            .state_mut(slot)
            .ok_or(MemoryError::Slot)?
            .pages
            .try_reserve(added as usize)?;
        self.mmu.set_extent(space, top)?;
        if let Err(error) = self.reserve_slot_tables(slot, top) {
            let _ = self.mmu.set_extent(space, old);
            return Err(error);
        }
        for page in 0..added {
            if let Err(error) = self.add_slot_page(slot, space) {
                // Back to the old top, extent included.
                self.shrink_slot(slot, page);
                return Err(error);
            }
        }
        Ok(())
    }

    fn add_slot_page(&mut self, slot: Slot, space: Space) -> Result<(), MemoryError> {
        let index = self.slots.state(slot).map_or(0, |state| state.pages.len());
        let index = u32::try_from(index).map_err(|_| MemoryError::NoMemory)?;
        let frame = self.take_page(Holder::Slot {
            slot: slot.index,
            index,
        })?;
        let address = APPLICATION + index * PAGE_SIZE;
        let mapped = self
            .mmu
            .map(Table::Space(space), address, frame, APPLICATION_ACCESS);
        let _ = self.mmu.release(frame);
        if let Err(error) = mapped {
            self.pages.free(frame);
            return Err(error.into());
        }
        if let Some(state) = self.slots.state_mut(slot) {
            state.pages.push(frame);
        }
        Ok(())
    }

    /// Takes `pages` from the top of a slot's application space.
    pub(super) fn shrink_slot(&mut self, slot: Slot, pages: u32) {
        let Some(state) = self.slots.state_mut(slot) else {
            return;
        };
        let space = state.space;
        let keep = state.pages.len().saturating_sub(pages as usize);
        let gone: Vec<Frame> = state.pages.drain(keep..).collect();
        let top = state.top();
        for (offset, frame) in (0u32..).zip(gone) {
            let address = top + offset * PAGE_SIZE;
            self.pages.free(frame);
            let _ = self.mmu.unmap(Table::Space(space), address);
        }
        let _ = self.reserve_slot_tables(slot, top);
        let _ = self.mmu.set_extent(space, top);
    }

    /// Reserves, or lets go of, a slot's tables so exactly those mapping
    /// below `top` are reserved, each once.
    fn reserve_slot_tables(&mut self, slot: Slot, top: u32) -> Result<(), MemoryError> {
        let state = self.slots.state(slot).ok_or(MemoryError::Slot)?;
        let (space, have) = (state.space, state.tables);
        let want = top.div_ceil(TABLE_SPAN);
        let table = Table::Space(space);
        if want > have {
            self.mmu
                .reserve(table, have * TABLE_SPAN..want * TABLE_SPAN)?;
        } else if want < have {
            self.mmu
                .unreserve(table, want * TABLE_SPAN..have * TABLE_SPAN);
        }
        if let Some(state) = self.slots.state_mut(slot) {
            state.tables = want;
        }
        Ok(())
    }

    /// Sets the application-space limit (`OS_DynamicArea` 26), returning the
    /// one before. Zero, or the limit as it is, only reads it.
    pub fn set_application_limit(&mut self, limit: u32) -> Result<u32, super::AreaError> {
        let old = self.application_limit();
        if limit == 0 || limit == old {
            return Ok(old);
        }
        if !limit.is_multiple_of(PAGE_SIZE)
            || limit < APPLICATION + PAGE_SIZE
            || limit > self.layout.top_32bit
        {
            return Err(super::AreaError::BadParameters);
        }
        let blocked = if limit > old {
            self.areas
                .lowest_reservation_above(APPLICATION)
                .is_some_and(|start| start < limit)
        } else {
            limit < self.slots.highest_top()
        };
        if blocked {
            return Err(super::AreaError::OverlappingAreas);
        }
        self.slots.limit = Some(limit);
        Ok(old)
    }
}
