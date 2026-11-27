//! `OS_Heap`'s heaps (`plans/RISCOS-EMULATOR.md` decision 6): in guest
//! memory in RISC OS's format, each reason performed as RISC OS 5's heap
//! manager performs it.
//!
//! A heap starts with a 16-byte descriptor: the magic word, the offset from
//! `heap + 4` of the first free block, the offset of the high-water mark
//! and the heap's size. A free block holds the signed offset to the next
//! and its size; an allocated block holds its size, header included, before
//! the bytes it hands out. Free blocks are listed in ascending address
//! order.
//!
//! The heap is the guest's, so every link and size is checked before it is
//! followed: a free list must ascend inside the used part, and blocks must
//! tile it. A heap that breaks either is reported overwritten, never walked
//! into.

use core::num::NonZeroU32;

use crate::error::Error;

/// The descriptor's magic word: the bytes `Heap`.
pub const MAGIC: u32 = 0x7061_6548;
/// The descriptor's size: where the first block may start.
const DESCRIPTOR: u32 = 16;
/// The smallest heap `OS_Heap` 0 makes.
const SMALLEST: u32 = 24;
/// The largest remnant a split absorbs rather than leave free.
const FRAGMENT: u32 = 8;

/// Why a heap operation failed: RISC OS's error for it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HeapError {
    /// "No such reason code".
    BadReason,
    /// "Unable to initialise the heap".
    FailInit,
    /// "Not a valid heap".
    BadDescriptor,
    /// "Heap structure is corrupt".
    BadLink,
    /// "No room in the heap for the block".
    NoRoom,
    /// "Not a heap block".
    NotABlock,
    /// "No memory to extend the heap into".
    BadExtend,
    /// "The heap cannot shrink that far": the heap shrank by `removed` bytes.
    ExcessiveShrink {
        /// The bytes it did shrink by.
        removed: u32,
    },
}

impl HeapError {
    /// The error RISC OS gives for it.
    #[must_use]
    pub const fn error(self) -> Error {
        let (number, token) = match self {
            Self::BadReason => (0x180, "UnknownReasonCode"),
            Self::FailInit => (0x181, "HeapNotInitialised"),
            Self::BadDescriptor => (0x182, "BadHeapDescriptor"),
            Self::BadLink => (0x183, "HeapCorrupt"),
            Self::NoRoom => (0x184, "HeapFull"),
            Self::NotABlock => (0x185, "NotAHeapBlock"),
            Self::BadExtend => (0x186, "HeapCannotGrow"),
            Self::ExcessiveShrink { .. } => (0x187, "HeapShrinkLimited"),
        };
        Error { number, token }
    }
}

/// Guest memory as a heap reaches it.
pub trait Space {
    /// The word at `address`, a multiple of four; `None` where it cannot be
    /// read.
    fn read(&mut self, address: u32) -> Option<u32>;
    /// Writes the word `value` at `address`; `None` where it cannot be.
    fn write(&mut self, address: u32, value: u32) -> Option<()>;
    /// Whether every byte from `start` to `end` may be read and written.
    fn valid(&mut self, start: u32, end: u64) -> bool;
}

/// A free block: its address, its size, and the address of the link that
/// names it, whose offset is from that address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Free {
    at: u32,
    size: u32,
    slot: u32,
}

impl Free {
    const fn end(self) -> u32 {
        self.at + self.size
    }
}

/// An allocated block found among its neighbours: the free blocks either
/// side of it.
#[derive(Clone, Copy, Debug)]
struct Found {
    at: u32,
    size: u32,
    before: Option<Free>,
    after: Option<Free>,
    /// The link naming `after`, or that would name a block in its place:
    /// `before`'s, or the descriptor's.
    after_slot: u32,
}

/// A heap at an address, with the bounds its descriptor gives.
#[derive(Clone, Copy, Debug)]
pub struct Heap {
    at: u32,
    base: u32,
    end: u32,
}

/// The offset from `slot` to `target`, as a link stores it.
const fn offset(slot: u32, target: u32) -> u32 {
    target.wrapping_sub(slot)
}

fn read(space: &mut dyn Space, address: u32) -> Result<u32, HeapError> {
    space.read(address).ok_or(HeapError::BadLink)
}

fn write(space: &mut dyn Space, address: u32, value: u32) -> Result<(), HeapError> {
    space.write(address, value).ok_or(HeapError::BadLink)
}

/// `OS_Heap` 0: a heap of `size` bytes at `at`.
pub fn initialise(space: &mut dyn Space, at: u32, size: u32) -> Result<(), HeapError> {
    if size < SMALLEST || !at.is_multiple_of(4) || !space.valid(at, u64::from(at) + u64::from(size))
    {
        return Err(HeapError::FailInit);
    }
    for (index, value) in (0..).zip([MAGIC, 0, DESCRIPTOR, size]) {
        space
            .write(at + 4 * index, value)
            .ok_or(HeapError::FailInit)?;
    }
    Ok(())
}

impl Heap {
    /// The heap at `at`, its descriptor checked: aligned, the magic word, a
    /// high-water mark inside it, all of it valid memory.
    pub fn open(space: &mut dyn Space, at: u32) -> Result<Self, HeapError> {
        let readable = at.is_multiple_of(4) && space.valid(at, u64::from(at) + u64::from(SMALLEST));
        if !readable || space.read(at) != Some(MAGIC) {
            return Err(HeapError::BadDescriptor);
        }
        let base = space.read(at + 8).ok_or(HeapError::BadDescriptor)?;
        let end = space.read(at + 12).ok_or(HeapError::BadDescriptor)?;
        if !space.valid(at, u64::from(at) + u64::from(end)) {
            return Err(HeapError::BadDescriptor);
        }
        if base < DESCRIPTOR || base % 4 != 0 || base > end {
            return Err(HeapError::BadLink);
        }
        Ok(Self { at, base, end })
    }

    /// The bytes past the high-water mark.
    #[must_use]
    pub const fn room(&self) -> u32 {
        self.end - self.base
    }

    /// The heap's size.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.end
    }

    /// The address of the high-water mark.
    const fn top(&self) -> u32 {
        self.at + self.base
    }

    fn set_base(&mut self, space: &mut dyn Space, top: u32) -> Result<(), HeapError> {
        self.base = top - self.at;
        write(space, self.at + 8, self.base)
    }

    /// The free block `slot`'s link names, if it names one, checked to lie
    /// past `floor` and inside the used part.
    fn free_at(
        &self,
        space: &mut dyn Space,
        slot: u32,
        floor: u32,
    ) -> Result<Option<Free>, HeapError> {
        let link = read(space, slot)?;
        if link == 0 {
            return Ok(None);
        }
        let at = slot.wrapping_add(link);
        if link > 0x7FFF_FFFF || at % 4 != 0 || at < floor || at < slot {
            return Err(HeapError::BadLink);
        }
        let size = if at <= self.top() - 8 {
            read(space, at + 4)?
        } else {
            return Err(HeapError::BadLink);
        };
        if size < 8 || size % 4 != 0 || size > self.top() - at {
            return Err(HeapError::BadLink);
        }
        Ok(Some(Free { at, size, slot }))
    }

    /// The first free block.
    fn first(&self, space: &mut dyn Space) -> Result<Option<Free>, HeapError> {
        self.free_at(space, self.at + 4, self.at + DESCRIPTOR)
    }

    /// The free block after `free`.
    fn next(&self, space: &mut dyn Space, free: Free) -> Result<Option<Free>, HeapError> {
        self.free_at(space, free.at, free.end())
    }

    /// Points `slot`'s link at `target`, or ends the list there.
    fn link(space: &mut dyn Space, slot: u32, target: Option<u32>) -> Result<(), HeapError> {
        write(space, slot, target.map_or(0, |target| offset(slot, target)))
    }

    /// `OS_Heap` 1: the largest block a claim could get and the free bytes
    /// in all.
    pub fn describe(&self, space: &mut dyn Space) -> Result<(u32, u32), HeapError> {
        let top_area = self.end - self.base;
        let mut largest = top_area;
        let mut total = top_area;
        let mut free = self.first(space)?;
        while let Some(block) = free {
            largest = largest.max(block.size);
            total = total.saturating_add(block.size);
            free = self.next(space, block)?;
        }
        Ok((largest.saturating_sub(4), total))
    }

    /// `OS_Heap` 2: a block of at least `size` bytes, first fit by address,
    /// taken from the top of the free block that fits.
    pub fn get(&mut self, space: &mut dyn Space, size: u32) -> Result<u32, HeapError> {
        if size == 0 || size > 0x7FFF_FFFF {
            return Err(HeapError::NoRoom);
        }
        let whole = (size + 7) & !3;
        let mut free = self.first(space)?;
        while let Some(block) = free {
            let next = self.next(space, block)?;
            if block.size >= whole {
                let left = block.size - whole;
                return if left <= FRAGMENT {
                    Self::link(space, block.slot, next.map(|n| n.at))?;
                    write(space, block.at, block.size)?;
                    Ok(block.at + 4)
                } else {
                    write(space, block.at + 4, left)?;
                    write(space, block.at + left, whole)?;
                    Ok(block.at + left + 4)
                };
            }
            free = next;
        }
        self.take_from_top(space, whole)
    }

    /// A block of `whole` bytes from the top area.
    fn take_from_top(&mut self, space: &mut dyn Space, whole: u32) -> Result<u32, HeapError> {
        if whole > self.end - self.base {
            return Err(HeapError::NoRoom);
        }
        let at = self.top();
        write(space, at, whole)?;
        self.set_base(space, at + whole)?;
        Ok(at + 4)
    }

    /// The allocated block a pointer `user` hands out, and its free
    /// neighbours; "Not a heap block" for anything else.
    fn find(&self, space: &mut dyn Space, user: u32) -> Result<Found, HeapError> {
        let at = user.wrapping_sub(4);
        if !user.is_multiple_of(4) || at < self.at + DESCRIPTOR || at >= self.top() {
            return Err(HeapError::NotABlock);
        }
        let mut before = None;
        let mut after = self.first(space)?;
        let mut after_slot = self.at + 4;
        while let Some(block) = after {
            if block.at > at {
                break;
            }
            if block.at == at || block.end() > at {
                return Err(HeapError::NotABlock);
            }
            before = Some(block);
            after_slot = block.at;
            after = self.next(space, block)?;
        }
        let limit = after.map_or(self.top(), |block| block.at);
        let mut walk = before.map_or(self.at + DESCRIPTOR, Free::end);
        while walk < at {
            let size = read(space, walk)?;
            if size < 8 || size % 4 != 0 || size > limit - walk {
                return Err(HeapError::BadLink);
            }
            walk += size;
        }
        if walk != at {
            return Err(HeapError::NotABlock);
        }
        let size = read(space, at)?;
        if size < 8 || size % 4 != 0 || size > limit - at {
            return Err(HeapError::BadLink);
        }
        Ok(Found {
            at,
            size,
            before,
            after,
            after_slot,
        })
    }

    /// `OS_Heap` 3.
    pub fn free(&mut self, space: &mut dyn Space, user: u32) -> Result<(), HeapError> {
        let found = self.find(space, user)?;
        self.release(space, found.at, found.size, found)
    }

    /// Returns `size` bytes at `at` to the free list, merging with the free
    /// blocks either side and giving back to the top area whatever reaches
    /// the high-water mark.
    fn release(
        &mut self,
        space: &mut dyn Space,
        at: u32,
        size: u32,
        found: Found,
    ) -> Result<(), HeapError> {
        let mut start = at;
        let mut end = at + size;
        let mut next = found.after;
        if let Some(after) = found.after.filter(|after| after.at == end) {
            end = after.end();
            next = self.next(space, after)?;
        }
        let merged_before = found.before.filter(|before| before.end() == start);
        if let Some(before) = merged_before {
            start = before.at;
        }
        // The link that names the merged block's place in the list.
        let slot = merged_before.map_or(found.after_slot, |before| before.slot);
        if end == self.top() {
            Self::link(space, slot, None)?;
            return self.set_base(space, start);
        }
        Self::link(space, slot, Some(start))?;
        Self::link(space, start, next.map(|n| n.at))?;
        write(space, start + 4, end - start)
    }

    /// `OS_Heap` 6: the block's size, its header included.
    pub fn block_size(&self, space: &mut dyn Space, user: u32) -> Result<u32, HeapError> {
        Ok(self.find(space, user)?.size)
    }

    /// `OS_Heap` 4: the block at `user` resized by `change` bytes, rounded
    /// to a word away from zero for growth and toward it for shrinkage. The
    /// block's new address, or none if the shrink freed it.
    pub fn extend_block(
        &mut self,
        space: &mut dyn Space,
        user: u32,
        change: i32,
    ) -> Result<Option<u32>, HeapError> {
        let found = self.find(space, user)?;
        let change = change.wrapping_add(3) & !3;
        match change.cmp(&0) {
            core::cmp::Ordering::Equal => Ok(Some(user)),
            core::cmp::Ordering::Less => self.shrink_block(space, found, change.unsigned_abs()),
            core::cmp::Ordering::Greater => self.grow_block(space, found, change.unsigned_abs()),
        }
    }

    fn shrink_block(
        &mut self,
        space: &mut dyn Space,
        found: Found,
        by: u32,
    ) -> Result<Option<u32>, HeapError> {
        if by >= found.size - 4 {
            self.release(space, found.at, found.size, found)?;
            return Ok(None);
        }
        let end = found.at + found.size;
        let followed = found.after.is_some_and(|after| after.at == end) || end == self.top();
        if by == 4 && !followed {
            return Ok(Some(found.at + 4));
        }
        let keep = found.size - by;
        write(space, found.at, keep)?;
        let tail = Found {
            at: found.at + keep,
            size: by,
            before: None,
            ..found
        };
        self.release(space, tail.at, by, tail)?;
        Ok(Some(found.at + 4))
    }

    fn grow_block(
        &mut self,
        space: &mut dyn Space,
        found: Found,
        by: u32,
    ) -> Result<Option<u32>, HeapError> {
        let end = found.at + found.size;
        let after = found.after.filter(|after| after.at == end);
        let at_top = end == self.top();
        let room = match after {
            Some(after) => after.size,
            None if at_top => (self.end & !3) - self.base,
            None => 0,
        };
        if room >= by {
            self.grow_up(space, found, after, by)?;
            return Ok(Some(found.at + 4));
        }
        if let Some(before) = found.before.filter(|before| before.end() == found.at) {
            if u64::from(before.size) + u64::from(room) >= u64::from(by) {
                return self
                    .grow_down(space, found, before, after, room, by)
                    .map(Some);
            }
        }
        let user = self.get(space, found.size - 4 + by)?;
        copy(space, found.at + 4, user, found.size - 4)?;
        let old = self.find(space, found.at + 4)?;
        self.release(space, old.at, old.size, old)?;
        Ok(Some(user))
    }

    /// Grows the block in place into the free block or top area after it.
    fn grow_up(
        &mut self,
        space: &mut dyn Space,
        found: Found,
        after: Option<Free>,
        by: u32,
    ) -> Result<(), HeapError> {
        let end = found.at + found.size;
        let mut grown = by;
        match after {
            Some(after) => {
                let left = after.size - by;
                let next = self.next(space, after)?;
                if left <= 4 {
                    grown = after.size;
                    Self::link(space, after.slot, next.map(|n| n.at))?;
                } else {
                    let moved = end + by;
                    Self::link(space, after.slot, Some(moved))?;
                    Self::link(space, moved, next.map(|n| n.at))?;
                    write(space, moved + 4, left)?;
                }
            }
            None => self.set_base(space, end + by)?,
        }
        write(space, found.at, found.size + grown)
    }

    /// Grows the block down into the free block before it, taking all the
    /// room after it first; its bytes move down with it.
    fn grow_down(
        &mut self,
        space: &mut dyn Space,
        found: Found,
        before: Free,
        after: Option<Free>,
        room: u32,
        by: u32,
    ) -> Result<u32, HeapError> {
        let end = found.at + found.size;
        let next = match after {
            Some(after) => self.next(space, after)?,
            None => found.after,
        };
        let needed = by.saturating_sub(room);
        let taken = if before.size - needed < FRAGMENT {
            before.size
        } else {
            needed
        };
        let new_at = before.end() - taken;
        copy(space, found.at + 4, new_at + 4, found.size - 4)?;
        let after_end = match after {
            Some(after) => after.end(),
            None if room > 0 => {
                self.set_base(space, self.at + (self.end & !3))?;
                self.top()
            }
            None => end,
        };
        if taken == before.size {
            Self::link(space, before.slot, next.map(|n| n.at))?;
        } else {
            write(space, before.at + 4, before.size - taken)?;
            Self::link(space, before.at, next.map(|n| n.at))?;
        }
        write(space, new_at, after_end - new_at)?;
        Ok(new_at + 4)
    }

    /// `OS_Heap` 5: the heap's size changed by `change` bytes, rounded toward
    /// zero to a word; the change made.
    pub fn extend_heap(&mut self, space: &mut dyn Space, change: i32) -> Result<i32, HeapError> {
        let change = change - change % 4;
        let end = i64::from(self.end) + i64::from(change);
        if end < i64::from(self.base) {
            let removed = self.end - self.base;
            self.end = self.base;
            write(space, self.at + 12, self.end)?;
            return Err(HeapError::ExcessiveShrink { removed });
        }
        let end = u32::try_from(end).map_err(|_| HeapError::BadExtend)?;
        if !space.valid(self.at, u64::from(self.at) + u64::from(end)) {
            return Err(HeapError::BadExtend);
        }
        self.end = end;
        write(space, self.at + 12, end)?;
        Ok(change)
    }

    /// `OS_Heap` 7 and 8: a block of `size` bytes whose address is a multiple
    /// of `alignment`, a power of two or zero (at least 4 either way), and
    /// which crosses no multiple of `boundary` (none if 0), both judged as if
    /// the heap started at `skew`.
    pub fn get_aligned(
        &mut self,
        space: &mut dyn Space,
        size: u32,
        alignment: u32,
        boundary: u32,
        skew: u32,
    ) -> Result<u32, HeapError> {
        let size = size.checked_add(3).ok_or(HeapError::NoRoom)? & !3;
        let bad_alignment = alignment != 0 && !alignment.is_power_of_two();
        let alignment = alignment.max(4);
        let bad_boundary = boundary != 0
            && (!boundary.is_power_of_two() || boundary < alignment || boundary < size);
        if size == 0
            || bad_alignment
            || bad_boundary
            || !skew.wrapping_sub(self.at).is_multiple_of(4)
        {
            return Err(HeapError::NoRoom);
        }
        let shape = Shape {
            size,
            alignment,
            boundary: NonZeroU32::new(boundary),
            delta: skew.wrapping_sub(self.at),
        };
        let mut free = self.first(space)?;
        let mut last_slot = self.at + 4;
        while let Some(block) = free {
            let next = self.next(space, block)?;
            if let Some(user) = shape.place(block.at, block.end()) {
                return Self::carve(space, block, next, user, size);
            }
            last_slot = block.at;
            free = next;
        }
        let top = self.top();
        let user = shape
            .place(top, self.at + (self.end & !3))
            .ok_or(HeapError::NoRoom)?;
        let gap = user - 4 - top;
        if gap > 0 {
            Self::link(space, last_slot, Some(top))?;
            Self::link(space, top, None)?;
            write(space, top + 4, gap)?;
        }
        write(space, user - 4, size + 4)?;
        self.set_base(space, user + size)?;
        Ok(user)
    }

    /// Takes the block at `user` out of the free block `block`: the gap
    /// before it stays free in `block`'s place, the remnant after it is
    /// freed unless too small to be.
    fn carve(
        space: &mut dyn Space,
        block: Free,
        next: Option<Free>,
        user: u32,
        size: u32,
    ) -> Result<u32, HeapError> {
        let gap = user - 4 - block.at;
        let tail_at = user + size;
        let tail = block.end() - tail_at;
        let (length, after) = if tail < FRAGMENT {
            (size + 4 + tail, next.map(|n| n.at))
        } else {
            Self::link(space, tail_at, next.map(|n| n.at))?;
            write(space, tail_at + 4, tail)?;
            (size + 4, Some(tail_at))
        };
        if gap == 0 {
            Self::link(space, block.slot, after)?;
        } else {
            write(space, block.at + 4, gap)?;
            Self::link(space, block.at, after)?;
        }
        write(space, user - 4, length)?;
        Ok(user)
    }
}

/// What an aligned claim asks for, its addresses judged `delta` bytes from
/// where they lie.
struct Shape {
    size: u32,
    alignment: u32,
    boundary: Option<NonZeroU32>,
    delta: u32,
}

impl Shape {
    /// The lowest pointer within a free run from `start` to `end` that the
    /// shape allows: aligned, leaving a gap before its header of none or a
    /// free block's worth, crossing no boundary.
    fn place(&self, start: u32, end: u32) -> Option<u32> {
        let skewed = |address: u32| address.wrapping_add(self.delta);
        let up = |address: u32, to: u32| {
            let s = skewed(address);
            let rounded = s.checked_add(to - 1)? & !(to - 1);
            address.checked_add(rounded - s)
        };
        let mut user = up(start + 4, self.alignment)?;
        loop {
            let gap = user - 4 - start;
            if gap != 0 && gap < FRAGMENT {
                user = up(user + 1, self.alignment)?;
                continue;
            }
            if let Some(boundary) = self.boundary {
                let first = skewed(user) / boundary;
                let last = skewed(user + self.size - 1) / boundary;
                if first != last {
                    user = up(user + 1, boundary.get())?;
                    continue;
                }
            }
            return (u64::from(user) + u64::from(self.size) <= u64::from(end)).then_some(user);
        }
    }
}

/// Copies `bytes` (a multiple of four) from `from` to `to`, which is lower
/// or equal, so an overlapping copy moves each word before overwriting it.
fn copy(space: &mut dyn Space, from: u32, to: u32, bytes: u32) -> Result<(), HeapError> {
    for offset in (0..bytes).step_by(4) {
        let word = read(space, from + offset)?;
        write(space, to + offset, word)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
