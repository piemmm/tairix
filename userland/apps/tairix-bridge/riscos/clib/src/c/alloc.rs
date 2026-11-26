//! The C library's heap, in the program's memory as RISC OS 5.30's is.
//!
//! A block is eight bytes of header — a guard word, then its size, the low
//! bit set while it is free — and then the size, a multiple of four, so the
//! next block's header follows it. A freed block's first word links the next
//! free block of its list: one list for each size up to 128, reused last-in
//! first-out as genuine's are, and one for each power of two above. Where no
//! free block serves and enough has been freed since, the free blocks next
//! to each other are merged; then the heap grows by `_kernel_alloc`. An
//! arena the heap's next growth cannot continue ends in a block leading to
//! the next.
//!
//! Nothing the client could have overwritten is trusted: a block whose size
//! no block could have, a list's block that is not a free block of its
//! size, a top outside the last arena, or a walk that leaves the heap or
//! comes round again, ends the program as a heap overwritten rather than
//! spreading the damage.

use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::report;
use super::statics::{at, HEAP_LIST_COUNT, HEAP_LIST_MAP_WORDS};
use crate::kernel::frame::readable;
use crate::kernel::statics::{at as kernel_at, Statics};
use crate::kernel::{context, heap};
use crate::lap::Lap;

/// A header's guard word: `"<Li>"`.
const GUARD: u32 = 0x3E69_4C3C;
/// The size word's bit marking a free block, and the one marking an arena's
/// end.
const FREE: u32 = 1;
const ARENA_END: u32 = 2;
const SIZE_BITS: u32 = !(FREE | ARENA_END);
const HEADER: u32 = 8;
/// The least size a block has.
const LEAST: u32 = 4;
/// The largest size with a list of its own; above it a list holds the sizes
/// from a power of two to the next.
const EXACT_MOST: u32 = 128;
const EXACT_LISTS: u32 = EXACT_MOST / LEAST;
/// The block ending an arena: its header, the next arena's start and a word
/// to spare. The part of the last arena not yet made blocks always keeps
/// room for one.
const ARENA_END_BLOCK: u32 = HEADER + 8;
/// The least an arena is made of: its end block and the least block.
const ARENA_SMALLEST: u32 = ARENA_END_BLOCK + HEADER + LEAST;

const _: () = assert!(EXACT_LISTS + 32 - EXACT_MOST.ilog2() == HEAP_LIST_COUNT);

/// Why the heap stopped: its misuse, or the call's own failure.
enum Stop {
    Misuse(Misuse),
    Fail(Fail),
}

impl From<Fail> for Stop {
    fn from(fail: Fail) -> Self {
        Self::Fail(fail)
    }
}

impl From<tairix_riscos_kernel::os::Unwind> for Stop {
    fn from(unwind: tairix_riscos_kernel::os::Unwind) -> Self {
        Self::Fail(Fail::Unwind(unwind))
    }
}

/// What a client did wrong to the heap.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Misuse {
    /// A block freed that the heap does not hold, or holds free already.
    Free,
    /// A block freed whose header has been overwritten.
    FreeOverwritten,
    /// The heap's own words overwritten, met while looking for room.
    Overwritten,
}

/// A block, by its header's address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Block(u32);

impl Block {
    const fn of_data(data: u32) -> Self {
        Self(data.wrapping_sub(HEADER))
    }

    const fn data(self) -> u32 {
        self.0.wrapping_add(HEADER)
    }

    /// The block after this one, of `size`.
    const fn after(self, size: u32) -> Self {
        Self(self.0.wrapping_add(HEADER).wrapping_add(size))
    }

    /// Its header, where the caller may read it.
    fn header(self, os: &mut Os<'_>) -> Option<(u32, u32)> {
        Some((readable(os, self.0)?, readable(os, self.0.wrapping_add(4))?))
    }

    fn mark(self, os: &mut Os<'_>, size_word: u32) -> Result<(), Fail> {
        os.as_caller().write_words(self.0, &[GUARD, size_word])
    }

    fn link(self, os: &mut Os<'_>) -> Result<u32, Fail> {
        os.as_caller().read_word(self.data())
    }

    fn set_link(self, os: &mut Os<'_>, next: u32) -> Result<(), Fail> {
        os.as_caller().write_word(self.data(), next)
    }
}

/// The heap of a client's statics.
#[derive(Clone, Copy)]
struct Heap(Statics);

/// The free list of blocks of `size`.
const fn list_of(size: u32) -> u32 {
    if size <= EXACT_MOST {
        size / LEAST - 1
    } else {
        EXACT_LISTS + size.ilog2() - EXACT_MOST.ilog2()
    }
}

impl Heap {
    fn read(self, os: &mut Os<'_>, at: u32) -> Result<u32, Fail> {
        self.0.read(os, at)
    }

    fn write(self, os: &mut Os<'_>, at: u32, value: u32) -> Result<(), Fail> {
        self.0.write(os, at, value)
    }

    fn head(self, list: u32) -> u32 {
        self.0.field(at::HEAP_LISTS + 4 * list)
    }

    fn map_word(list: u32) -> u32 {
        at::HEAP_LIST_MAP + 4 * (list / 32)
    }

    fn set_listed(self, os: &mut Os<'_>, list: u32, listed: bool) -> Result<(), Fail> {
        let at = Self::map_word(list);
        let map = self.read(os, at)?;
        let bit = 1 << (list % 32);
        self.write(os, at, if listed { map | bit } else { map & !bit })
    }

    /// The first list from `list` on with blocks.
    fn listed_from(self, os: &mut Os<'_>, list: u32) -> Result<Option<u32>, Fail> {
        for word in list / 32..HEAP_LIST_MAP_WORDS {
            let mut map = self.read(os, at::HEAP_LIST_MAP + 4 * word)?;
            if word == list / 32 {
                map &= !((1 << (list % 32)) - 1);
            }
            if map != 0 {
                return Ok(Some(32 * word + map.trailing_zeros()));
            }
        }
        Ok(None)
    }

    /// Whether a block may lie at `address`, word-aligned: within the span
    /// an application's arenas cover, one after another; anywhere for a
    /// module's, which lie about the RMA.
    fn may_hold(self, os: &mut Os<'_>, address: u32) -> Result<bool, Fail> {
        if !address.is_multiple_of(4) {
            return Ok(false);
        }
        if self.read(os, kernel_at::MODULE)? != 0 {
            return Ok(true);
        }
        let first = self.read(os, at::HEAP_FIRST)?;
        Ok((first..self.read(os, at::HEAP_END)?).contains(&address))
    }

    /// Whether a block of `size` may follow a header [`Self::may_hold`]
    /// allows at `block`: the least size at least, its end not wrapping and,
    /// for an application's, within its arenas' span.
    fn spans(self, os: &mut Os<'_>, block: Block, size: u32) -> Result<bool, Fail> {
        let end = block
            .0
            .checked_add(HEADER)
            .and_then(|data| data.checked_add(size));
        let Some(end) = end.filter(|_| size >= LEAST) else {
            return Ok(false);
        };
        if self.read(os, kernel_at::MODULE)? != 0 {
            return Ok(true);
        }
        Ok(end <= self.read(os, at::HEAP_END)?)
    }

    /// The most steps a walk of the heap can take before it has met every
    /// block twice.
    fn steps_most(self, os: &mut Os<'_>) -> Result<u32, Fail> {
        Ok(self.read(os, at::HEAP_BYTES)? / (HEADER + LEAST) + 1)
    }

    /// `block`, of `size` and at least the least size, marked free and put
    /// at the head of its list.
    fn push(self, os: &mut Os<'_>, block: Block, size: u32) -> Result<(), Fail> {
        let list = list_of(size);
        block.mark(os, size | FREE)?;
        let next = os.as_caller().read_word(self.head(list))?;
        block.set_link(os, next)?;
        os.as_caller().write_word(self.head(list), block.0)?;
        self.set_listed(os, list, true)
    }

    /// The free block `link` holds, checked as one of `list`'s: its size.
    fn listed(self, os: &mut Os<'_>, block: Block, list: u32) -> Result<u32, Stop> {
        let header = if self.may_hold(os, block.0)? {
            block.header(os)
        } else {
            None
        };
        let Some((guard, word)) = header else {
            return Err(Stop::Misuse(Misuse::Overwritten));
        };
        let size = word & SIZE_BITS;
        let sound =
            guard == GUARD && word & (FREE | ARENA_END) == FREE && self.spans(os, block, size)?;
        if !sound || list_of(size) != list {
            return Err(Stop::Misuse(Misuse::Overwritten));
        }
        Ok(size)
    }

    /// `block`, of `size`, made a block of `wanted` and handed out, what is
    /// left freed where it can be a block of its own.
    fn hand_out(self, os: &mut Os<'_>, block: Block, size: u32, wanted: u32) -> Result<u32, Fail> {
        if size - wanted >= HEADER + LEAST {
            block.mark(os, wanted)?;
            self.push(os, block.after(wanted), size - wanted - HEADER)?;
        } else {
            block.mark(os, size)?;
        }
        Ok(block.data())
    }

    /// A free block of at least `wanted` taken: the head of the first list
    /// from `wanted`'s own on that serves, within `wanted`'s own list the
    /// first block that does.
    fn take_free(self, os: &mut Os<'_>, wanted: u32) -> Result<Option<u32>, Stop> {
        let own = list_of(wanted);
        let mut from = own;
        while let Some(list) = self.listed_from(os, from)? {
            let mut link = self.head(list);
            let mut steps = self.steps_most(os)?;
            let mut lap = Lap::from(0);
            loop {
                let block = Block(os.as_caller().read_word(link)?);
                if block.0 == 0 {
                    break;
                }
                if lap.repeated(block.0) {
                    return Err(Stop::Misuse(Misuse::Overwritten));
                }
                let size = self.listed(os, block, list)?;
                if size >= wanted {
                    let next = block.link(os)?;
                    os.as_caller().write_word(link, next)?;
                    if os.as_caller().read_word(self.head(list))? == 0 {
                        self.set_listed(os, list, false)?;
                    }
                    return Ok(Some(self.hand_out(os, block, size, wanted)?));
                }
                steps = steps
                    .checked_sub(1)
                    .ok_or(Stop::Misuse(Misuse::Overwritten))?;
                link = block.data();
                os.yield_point()?;
            }
            if list != own {
                return Err(Stop::Misuse(Misuse::Overwritten));
            }
            from = list + 1;
        }
        Ok(None)
    }

    /// The room in the last arena not yet made blocks, before the block that
    /// will end it: its start and its length.
    fn unmade(self, os: &mut Os<'_>) -> Result<(Block, u32), Stop> {
        let top = self.read(os, at::HEAP_TOP)?;
        let room = self
            .read(os, at::HEAP_END)?
            .checked_sub(ARENA_END_BLOCK)
            .and_then(|end| end.checked_sub(top));
        match room {
            Some(room) if self.may_hold(os, top)? => Ok((Block(top), room)),
            _ => Err(Stop::Misuse(Misuse::Overwritten)),
        }
    }

    /// A block of `wanted` made from the part of the last arena not yet made
    /// blocks, a remainder too small to be a block given to it.
    fn carve(self, os: &mut Os<'_>, wanted: u32) -> Result<Option<u32>, Stop> {
        let (block, room) = self.unmade(os)?;
        let Some(left) = room.checked_sub(HEADER).and_then(|r| r.checked_sub(wanted)) else {
            return Ok(None);
        };
        let size = if left < HEADER + LEAST {
            wanted + left
        } else {
            wanted
        };
        block.mark(os, size)?;
        self.write(os, at::HEAP_TOP, block.after(size).0)?;
        Ok(Some(block.data()))
    }

    /// The heap walked from its first arena, each run of free blocks merged
    /// into one and every list rebuilt; a run reaching the last arena's
    /// unmade part joins it.
    fn merge(self, os: &mut Os<'_>) -> Result<(), Stop> {
        self.write(os, at::HEAP_FREED, 0)?;
        os.as_caller()
            .fill(self.0.field(at::HEAP_LIST_MAP), 4 * HEAP_LIST_MAP_WORDS, 0)?;
        os.as_caller().fill(self.head(0), 4 * HEAP_LIST_COUNT, 0)?;
        let top = self.read(os, at::HEAP_TOP)?;
        let mut steps = self.steps_most(os)?;
        let mut block = Block(self.read(os, at::HEAP_FIRST)?);
        let mut lap = Lap::from(block.0);
        let mut run: Option<Block> = None;
        while block.0 != top {
            steps = steps
                .checked_sub(1)
                .ok_or(Stop::Misuse(Misuse::Overwritten))?;
            let header = if self.may_hold(os, block.0)? {
                block.header(os)
            } else {
                None
            };
            let Some((GUARD, word)) = header else {
                return Err(Stop::Misuse(Misuse::Overwritten));
            };
            let next = if word & ARENA_END == 0 {
                let size = word & SIZE_BITS;
                self.spans(os, block, size)?.then(|| block.after(size))
            } else {
                readable(os, block.data()).map(Block)
            };
            let next = next
                .filter(|next| !lap.repeated(next.0))
                .ok_or(Stop::Misuse(Misuse::Overwritten))?;
            if word & (FREE | ARENA_END) == FREE {
                run.get_or_insert(block);
            } else if let Some(start) = run.take() {
                // An arena's blocks lie one after another, each spanning at
                // least the least size, so a run ends past its start.
                self.push(os, start, block.0 - start.0 - HEADER)?;
            }
            block = next;
            os.yield_point()?;
        }
        if let Some(start) = run {
            self.write(os, at::HEAP_TOP, start.0)?;
        }
        Ok(())
    }

    /// The heap grown for a block of `wanted`: the last arena continued
    /// where the new memory follows it, else a new arena the old one's end
    /// block leads to.
    fn grow(self, os: &mut Os<'_>, wanted: u32) -> Result<bool, Stop> {
        let Some(bytes) = wanted.checked_add(HEADER + ARENA_END_BLOCK) else {
            return Ok(false);
        };
        let (top, left) = self.unmade(os)?;
        let old_end = self.read(os, at::HEAP_END)?;
        let Some((start, length)) = claim_arena(os, self.0, bytes)? else {
            return Ok(false);
        };
        if start != old_end && length < ARENA_SMALLEST {
            return Ok(false);
        }
        let bytes_held = self.read(os, at::HEAP_BYTES)?;
        self.write(os, at::HEAP_BYTES, bytes_held.saturating_add(length))?;
        if start != old_end {
            let marker = if left >= HEADER + LEAST {
                self.push(os, top, left - HEADER)?;
                top.after(left - HEADER)
            } else {
                top
            };
            marker.mark(os, (ARENA_END_BLOCK - HEADER) | ARENA_END)?;
            marker.set_link(os, start)?;
            self.write(os, at::HEAP_TOP, start)?;
        }
        self.write(os, at::HEAP_END, start.wrapping_add(length))?;
        Ok(true)
    }

    /// A block of `size`: its data's address, none where the heap has no
    /// room even grown, or was never begun.
    fn allocate(self, os: &mut Os<'_>, size: u32) -> Result<Option<u32>, Stop> {
        if self.read(os, at::HEAP_FIRST)? == 0 {
            return Ok(None);
        }
        if let Some(data) = self.take_free(os, size)? {
            return Ok(Some(data));
        }
        if let Some(data) = self.carve(os, size)? {
            return Ok(Some(data));
        }
        if self.read(os, at::HEAP_FREED)? >= size {
            self.merge(os)?;
            if let Some(data) = self.take_free(os, size)? {
                return Ok(Some(data));
            }
            if let Some(data) = self.carve(os, size)? {
                return Ok(Some(data));
            }
        }
        Ok(if self.grow(os, size)? {
            self.carve(os, size)?
        } else {
            None
        })
    }

    /// The block whose data is at `data`, checked as one the heap holds and
    /// has handed out: its size.
    fn given(self, os: &mut Os<'_>, data: u32) -> Result<u32, Stop> {
        let block = Block::of_data(data);
        let header = if self.may_hold(os, block.0)? {
            block.header(os)
        } else {
            None
        };
        let Some((guard, word)) = header else {
            return Err(Stop::Misuse(Misuse::Free));
        };
        if guard != GUARD {
            return Err(Stop::Misuse(Misuse::FreeOverwritten));
        }
        if word & (FREE | ARENA_END) != 0 {
            return Err(Stop::Misuse(Misuse::Free));
        }
        if !self.spans(os, block, word)? {
            return Err(Stop::Misuse(Misuse::FreeOverwritten));
        }
        Ok(word)
    }

    /// Walks a module's heap from `first`, its first arena, to its top,
    /// handing `each` every arena's start once the walk has read all it
    /// needs of it: whether the walk reached the top, every block sound,
    /// within the steps a sound heap takes.
    fn walk_arenas<'o>(
        self,
        os: &mut Os<'o>,
        first: u32,
        mut each: impl FnMut(&mut Os<'o>, u32) -> Result<(), Fail>,
    ) -> Result<bool, Fail> {
        let top = self.read(os, at::HEAP_TOP)?;
        let mut steps = self.steps_most(os)?;
        let mut arena = first;
        let mut block = Block(arena);
        let mut lap = Lap::from(arena);
        while block.0 != top {
            let Some(left) = steps.checked_sub(1) else {
                return Ok(false);
            };
            steps = left;
            let Some((GUARD, word)) = block.header(os) else {
                return Ok(false);
            };
            let next = if word & ARENA_END == 0 {
                let size = word & SIZE_BITS;
                if !self.spans(os, block, size)? {
                    return Ok(false);
                }
                block.after(size)
            } else {
                let Some(next) = readable(os, block.data()) else {
                    return Ok(false);
                };
                each(os, arena)?;
                arena = next;
                Block(next)
            };
            if lap.repeated(next.0) {
                return Ok(false);
            }
            block = next;
            os.yield_point()?;
        }
        each(os, arena)?;
        Ok(true)
    }

    /// A block of `size`, as `malloc` answers: its data, or nought where
    /// there is no room or no size at all.
    fn malloc(self, os: &mut Os<'_>, size: Option<u32>) -> Result<u32, Stop> {
        Ok(match size {
            Some(size) => self.allocate(os, size)?.unwrap_or(0),
            None => 0,
        })
    }

    /// `realloc`'s block at `data` resized to `n` bytes: kept in place where
    /// it shrinks, or grows into the part of the heap not yet made blocks
    /// that it ends at; else moved, as `malloc` would size it. Where it now
    /// is, or nought where it is freed or there is no room for it.
    fn reallocate(self, os: &mut Os<'_>, data: u32, n: u32) -> Result<u32, Stop> {
        if data == 0 {
            return self.malloc(os, malloc_size(n));
        }
        let size = self.given(os, data)?;
        if n == 0 {
            self.release(os, data, size)?;
            return Ok(0);
        }
        let Some(kept) = realloc_size(n) else {
            return Ok(0);
        };
        let block = Block::of_data(data);
        if kept <= size {
            self.hand_out(os, block, size, kept)?;
            return Ok(data);
        }
        let after = block.after(size).0;
        if after == self.read(os, at::HEAP_TOP)? {
            self.write(os, at::HEAP_TOP, block.0)?;
            if self.carve(os, kept)?.is_some() {
                return Ok(data);
            }
            self.write(os, at::HEAP_TOP, after)?;
        }
        let moved = self.malloc(os, malloc_size(n))?;
        if moved != 0 {
            os.as_caller().copy_bytes(data, moved, size)?;
            self.release(os, data, size)?;
        }
        Ok(moved)
    }

    /// The block at `data`, of `size`, freed.
    fn release(self, os: &mut Os<'_>, data: u32, size: u32) -> Result<(), Fail> {
        let freed = self.read(os, at::HEAP_FREED)?;
        self.write(os, at::HEAP_FREED, freed.saturating_add(size + HEADER))?;
        self.push(os, Block::of_data(data), size)
    }
}

/// The size `malloc` gives a request of `n` bytes.
fn malloc_size(n: u32) -> Option<u32> {
    Some(n.checked_add(3)? & !3).map(|size| size.max(LEAST))
}

/// The size `realloc` gives a block it keeps in place: a word more than
/// `malloc`'s, as RISC OS 5.30's gives.
fn realloc_size(n: u32) -> Option<u32> {
    Some(n.checked_add(7)? & !3)
}

/// The least an arena is given: 2048 words, as `_kernel_alloc` gives.
const ARENA_LEAST: u32 = 4 * 2048;

/// Memory for an arena of at least `bytes`: an application's from the heap
/// `_kernel_alloc` takes from, a module's from the RMA. Its start and its
/// length; none where there is none.
fn claim_arena(os: &mut Os<'_>, statics: Statics, bytes: u32) -> Result<Option<(u32, u32)>, Fail> {
    if statics.read(os, kernel_at::MODULE)? == 0 {
        let (start, words) = heap::take(os, statics, bytes.div_ceil(4))?;
        return Ok((words != 0).then_some((start, 4 * words)));
    }
    let length = bytes.max(ARENA_LEAST).next_multiple_of(4);
    match os.rma_claim(length) {
        Ok(start) => Ok(Some((start, length))),
        Err(Fail::Error(_)) => Ok(None),
        Err(unwind) => Err(unwind),
    }
}

/// The heap begun: its first arena claimed and laid out. Whether there was
/// the memory.
pub(crate) fn begin(os: &mut Os<'_>, statics: Statics) -> Result<bool, Fail> {
    let claimed = claim_arena(os, statics, ARENA_LEAST)?;
    let Some((first, length)) = claimed.filter(|&(_, length)| length >= ARENA_SMALLEST) else {
        return Ok(false);
    };
    start(os, statics, first, length / 4)?;
    Ok(true)
}

/// The heap's first arena laid in the `words` words at `start`.
pub(crate) fn start(os: &mut Os<'_>, statics: Statics, start: u32, words: u32) -> Result<(), Fail> {
    for (field, value) in [
        (at::HEAP_FIRST, start),
        (at::HEAP_TOP, start),
        (at::HEAP_END, start.wrapping_add(4 * words)),
        (at::HEAP_FREED, 0),
        (at::HEAP_BYTES, 4 * words),
    ] {
        statics.write(os, field, value)?;
    }
    os.as_caller().fill(
        statics.field(at::HEAP_LIST_MAP),
        4 * (HEAP_LIST_MAP_WORDS + HEAP_LIST_COUNT),
        0,
    )
}

/// A module's heap given back to the RMA, arena by arena, once a walk has
/// found the whole of it sound: a heap whose walk goes astray keeps what is
/// left of it rather than freeing a block it may not own.
pub(crate) fn release(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    let heap = Heap(statics);
    let first = heap.read(os, at::HEAP_FIRST)?;
    if statics.read(os, kernel_at::MODULE)? == 0 || first == 0 {
        return Ok(());
    }
    if !heap.walk_arenas(os, first, |_, _| Ok(()))? {
        return Ok(());
    }
    heap.write(os, at::HEAP_FIRST, 0)?;
    heap.walk_arenas(os, first, Os::rma_free)?;
    Ok(())
}

/// `outcome`, or the program ended for its misuse of the heap.
fn settled<T>(
    os: &mut Os<'_>,
    statics: Statics,
    outcome: Result<T, Stop>,
) -> Result<Option<T>, Fail> {
    match outcome {
        Ok(value) => Ok(Some(value)),
        Err(Stop::Fail(fail)) => Err(fail),
        Err(Stop::Misuse(misuse)) => {
            let token = match misuse {
                Misuse::Free => "FreeFailed",
                Misuse::FreeOverwritten => "FreeOverwritten",
                Misuse::Overwritten => "HeapOverwritten",
            };
            report::unrecoverable(os, statics, token)?;
            Ok(None)
        }
    }
}

/// A block of `size` where there is one: its data, or nought.
fn allocate(os: &mut Os<'_>, statics: Statics, size: Option<u32>) -> Result<Option<u32>, Fail> {
    let allocated = Heap(statics).malloc(os, size);
    settled(os, statics, allocated)
}

/// A block of at least `bytes` kept for the library's own use at `field`
/// of the statics: the one there where it is large enough, else a new one,
/// the old freed; none where the heap has no room.
pub(crate) fn kept_buffer(
    os: &mut Os<'_>,
    statics: Statics,
    field: u32,
    bytes: u32,
) -> Result<Option<u32>, Fail> {
    let heap = Heap(statics);
    let kept = statics.read(os, field)?;
    if kept != 0 {
        let checked = heap.given(os, kept);
        let Some(size) = settled(os, statics, checked)? else {
            return Ok(None);
        };
        if size >= bytes {
            return Ok(Some(kept));
        }
        heap.release(os, kept, size)?;
        statics.write(os, field, 0)?;
    }
    let Some(data) = allocate(os, statics, malloc_size(bytes))?.filter(|&data| data != 0) else {
        return Ok(None);
    };
    statics.write(os, field, data)?;
    Ok(Some(data))
}

/// A block of `bytes` for the library's own use, as `malloc` gives one: its
/// data, or nought where the heap has no room.
pub(crate) fn allocated(os: &mut Os<'_>, statics: Statics, bytes: u32) -> Result<u32, Fail> {
    Ok(allocate(os, statics, malloc_size(bytes))?.unwrap_or(0))
}

/// The block at `data`, which the library had, given back as `free` gives
/// one back: nothing for nought.
pub(crate) fn freed(os: &mut Os<'_>, statics: Statics, data: u32) -> Result<(), Fail> {
    if data == 0 {
        return Ok(());
    }
    let heap = Heap(statics);
    let checked = heap.given(os, data);
    if let Some(size) = settled(os, statics, checked)? {
        heap.release(os, data, size)?;
    }
    Ok(())
}

/// `aligned_alloc(alignment, size)`: a block of `size + alignment + 8`
/// bytes; where its data is not on the alignment, the first aligned place at
/// least a free block's room past its start, the part before freed as a
/// block of its own, as RISC OS 5.30 splits it. Nought for an alignment
/// that is no power of two, or where the heap has no room. A size of nought
/// is asked as the least size, so the block aligned is one `free` can list.
pub(crate) fn aligned_alloc(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let (alignment, size) = (regs.r[0], regs.r[1]);
    regs.r[0] = 0;
    let request = size
        .max(LEAST)
        .checked_add(alignment)
        .and_then(|n| n.checked_add(HEADER));
    let Some(request) = request.filter(|_| alignment.is_power_of_two()) else {
        return Ok(());
    };
    let data = allocated(os, statics, request)?;
    if data == 0 || data.is_multiple_of(alignment) {
        regs.r[0] = data;
        return Ok(());
    }
    let heap = Heap(statics);
    let Some((_, word)) = Block::of_data(data).header(os) else {
        return Ok(());
    };
    let end = data.wrapping_add(word & SIZE_BITS);
    let mut aligned = data.next_multiple_of(alignment);
    while aligned - data < HEADER + LEAST {
        aligned += alignment;
    }
    heap.release(os, data, aligned - data - HEADER)?;
    Block::of_data(aligned).mark(os, end - aligned)?;
    regs.r[0] = aligned;
    Ok(())
}

/// The size of the block at `data`, where it is one the heap holds and has
/// handed out; none, and nothing reported, otherwise.
pub(crate) fn size_of(os: &mut Os<'_>, statics: Statics, data: u32) -> Result<Option<u32>, Fail> {
    match Heap(statics).given(os, data) {
        Ok(size) => Ok(Some(size)),
        Err(Stop::Misuse(_)) => Ok(None),
        Err(Stop::Fail(fail)) => Err(fail),
    }
}

/// The block at `data` marked free and nothing more, as `__rt_freeauto`
/// marks one: neither listed nor counted as freed, so only a merge takes it
/// back. Nothing for nought, or for anything that is not a block in use.
pub(crate) fn marked_free(os: &mut Os<'_>, statics: Statics, data: u32) -> Result<(), Fail> {
    if data == 0 {
        return Ok(());
    }
    match size_of(os, statics, data)? {
        Some(size) => Block::of_data(data).mark(os, size | FREE),
        None => Ok(()),
    }
}

/// `malloc(size)`, which the library kernel claims stack chunks through
/// too.
pub(crate) fn malloc(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    if let Some(data) = allocate(os, statics, malloc_size(regs.r[0]))? {
        regs.r[0] = data;
    }
    Ok(())
}

/// `calloc(nmemb, size)`: zeroed.
pub(super) fn calloc(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let bytes = regs.r[0].checked_mul(regs.r[1]);
    if let Some(data) = allocate(os, statics, bytes.and_then(malloc_size))? {
        if let (Some(bytes), true) = (bytes, data != 0) {
            os.as_caller().fill(data, bytes, 0)?;
        }
        regs.r[0] = data;
    }
    Ok(())
}

/// `free(ptr)`, which the library kernel frees stack chunks through too:
/// nothing for nought.
pub(crate) fn free(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    freed(os, statics, regs.r[0])
}

/// `realloc(ptr, size)`: kept in place where it shrinks, or grows into the
/// part of the heap not yet made blocks that it ends at; else moved, as
/// `malloc` would size it. A size of nought frees it, answering nought.
pub(super) fn realloc(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let resized = Heap(statics).reallocate(os, regs.r[0], regs.r[1]);
    if let Some(data) = settled(os, statics, resized)? {
        regs.r[0] = data;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
