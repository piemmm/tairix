//! The node arena the tree containers allocate from: nodes addressed by `u32`
//! index and held in chunks, so the arena grows a chunk at a time and never
//! moves a node to grow; the freed ones are chained through a link each node
//! carries, so a removal never allocates and an insertion reuses a freed node
//! before the arena grows.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::mem::{self, MaybeUninit};
use core::ops::{Index, IndexMut};

use crate::TryReserveError;

/// A link naming no node.
pub(crate) const NIL: u32 = u32::MAX;
/// A freed node's link past the last of its chain: a live node's link is
/// [`NIL`], so a freed one is told from it.
const FREE_END: u32 = NIL - 1;

/// The most bytes one chunk holds. A large arena is many allocations of at
/// most this size, so growing one never needs a large contiguous block and
/// never copies what it holds.
const CHUNK_BYTES: usize = 16 * 1024;

/// A node an arena chains through its link while it is freed, and makes
/// empty where it will lie.
pub(crate) trait Linked: Sized {
    fn link(&self) -> u32;
    fn set_link(&mut self, link: u32);
    /// Make `slot` an empty, live node, in place: a node may be far larger
    /// than a stack frame should hold, so one is never built elsewhere and
    /// moved in.
    fn write_empty(slot: &mut MaybeUninit<Self>);
}

/// An arena's nodes, chunk by chunk.
///
/// Chunk `0` holds one node and chunk `c`, for `1 <= c <= SHIFT`, holds
/// `2^(c-1)`, so a small arena holds at most twice the nodes it uses; every
/// later chunk holds `2^SHIFT`, as many as fit in [`CHUNK_BYTES`]. A chunk is
/// allocated whole, and every chunk below the last populated one is full.
///
/// The chunks and counts lie behind one pointer, allocated with the first
/// chunk and given back with the last: an arena costs the structure holding
/// it one word, and an empty one allocates nothing.
pub(crate) struct Nodes<N> {
    core: Option<Box<Core<N>>>,
}

/// A [`Nodes`]' chunks and counts, held while it holds a chunk.
struct Core<N> {
    chunks: Vec<Vec<N>>,
    len: usize,
    /// Slots the allocated chunks hold, populated or not.
    capacity: usize,
    /// The first freed node an [`Arena`] chains, and how many it chains:
    /// bounded by the `u32` index space, as every node is.
    free: u32,
    free_count: u32,
}

/// What a reservation changed, for [`Nodes::undo`] to put back: the chunks
/// and slots held before it, the list of chunks it outgrew, emptied, where
/// it grew one, and whether it made the core.
struct Undo<N> {
    chunks: usize,
    capacity: usize,
    outgrown: Option<Vec<Vec<N>>>,
    created: bool,
}

impl<N> Nodes<N> {
    /// Log2 of the nodes a full chunk holds.
    const SHIFT: u32 = {
        let size = if mem::size_of::<N>() == 0 {
            1
        } else {
            mem::size_of::<N>()
        };
        let fit = CHUNK_BYTES / size;
        if fit <= 1 {
            0
        } else {
            fit.ilog2()
        }
    };
    const FULL: usize = 1 << Self::SHIFT;

    const fn new() -> Self {
        Self { core: None }
    }

    /// The chunk holding arena index `index`, and its place there.
    #[inline]
    const fn locate(index: usize) -> (usize, usize) {
        if index >= Self::FULL {
            (
                Self::SHIFT as usize + (index >> Self::SHIFT),
                index & (Self::FULL - 1),
            )
        } else if index == 0 {
            (0, 0)
        } else {
            let chunk = index.ilog2() as usize + 1;
            (chunk, index - (1 << (chunk - 1)))
        }
    }

    /// The slots chunk `chunk` holds.
    const fn chunk_len(chunk: usize) -> usize {
        if chunk == 0 {
            1
        } else if chunk <= Self::SHIFT as usize {
            1 << (chunk - 1)
        } else {
            Self::FULL
        }
    }

    /// The slots chunks `0..chunks` hold together.
    const fn slots_below(chunks: usize) -> usize {
        if chunks <= Self::SHIFT as usize + 1 {
            if chunks == 0 {
                0
            } else {
                1 << (chunks - 1)
            }
        } else {
            (chunks - Self::SHIFT as usize) * Self::FULL
        }
    }

    /// The chunks `len` nodes populate.
    const fn populated(len: usize) -> usize {
        if len == 0 {
            0
        } else {
            Self::locate(len - 1).0 + 1
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.core.as_deref().map_or(0, |core| core.len)
    }

    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.core.as_deref().map_or(0, |core| core.capacity)
    }

    /// Whether it holds no chunk past the last it populates, no spare room in
    /// its list of chunks, and no core while it holds no chunk.
    #[cfg(test)]
    pub(crate) fn fitted(&self) -> bool {
        self.core.as_deref().is_none_or(|core| {
            !core.chunks.is_empty()
                && core.chunks.len() == Self::populated(core.len)
                && core.chunks.capacity() == core.chunks.len()
        })
    }

    #[inline]
    pub(crate) fn get(&self, index: usize) -> Option<&N> {
        self.view().get(index)
    }

    /// Its chunks, reached once for a descent that reads a node a level.
    #[inline]
    pub(crate) fn view(&self) -> NodesView<'_, N> {
        NodesView {
            chunks: self.core.as_deref().map_or(&[], |core| &core.chunks),
        }
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut N> {
        let (chunk, at) = Self::locate(index);
        self.core.as_deref_mut()?.chunks.get_mut(chunk)?.get_mut(at)
    }

    /// The nodes at `indices`, every one, or none at all where an index is
    /// past the end or repeated.
    pub(crate) fn disjoint_mut<const M: usize>(
        &mut self,
        indices: [usize; M],
    ) -> [Option<&mut N>; M] {
        let Some(core) = self.core.as_deref_mut() else {
            return [const { None }; M];
        };
        let len = core.len;
        let apart = indices
            .iter()
            .enumerate()
            .all(|(at, index)| *index < len && !indices[..at].contains(index));
        let mut found = [const { None }; M];
        if apart {
            Self::peel(&mut core.chunks, indices, &mut found);
        }
        if found.iter().any(Option::is_none) {
            return [const { None }; M];
        }
        found
    }

    /// Split the distinct, in-bounds `indices` off `chunks` in ascending
    /// order, each into its place in `found`.
    fn peel<'s, const M: usize>(
        chunks: &'s mut [Vec<N>],
        indices: [usize; M],
        found: &mut [Option<&'s mut N>; M],
    ) {
        let mut order: [usize; M] = core::array::from_fn(|at| at);
        order.sort_unstable_by_key(|&at| indices[at]);
        let mut rest = chunks;
        let mut next_chunk = 0;
        let mut open: Option<(usize, &'s mut [N], usize)> = None;
        for at in order {
            let (chunk, offset) = Self::locate(indices[at]);
            let (number, tail, start) = match open.take() {
                Some(held) if held.0 == chunk => held,
                _ => {
                    let Some(skip) = chunk.checked_sub(next_chunk) else {
                        return;
                    };
                    let Some((_, from)) = mem::take(&mut rest).split_at_mut_checked(skip) else {
                        return;
                    };
                    let Some((held, after)) = from.split_first_mut() else {
                        return;
                    };
                    rest = after;
                    next_chunk = chunk + 1;
                    (chunk, held.as_mut_slice(), 0)
                }
            };
            let Some(skip) = offset.checked_sub(start) else {
                return;
            };
            let Some((_, from)) = tail.split_at_mut_checked(skip) else {
                return;
            };
            let Some((node, after)) = from.split_first_mut() else {
                return;
            };
            found[at] = Some(node);
            open = Some((number, after, offset + 1));
        }
    }

    pub(crate) fn swap(&mut self, a: usize, b: usize) {
        if let [Some(first), Some(second)] = self.disjoint_mut([a, b]) {
            mem::swap(first, second);
        }
    }

    /// Make room for `additional` more nodes, a whole chunk at a time,
    /// answering what [`Self::undo`] needs to give the room back; a refusal
    /// leaves it holding exactly the room it had.
    fn reserve(&mut self, additional: usize) -> Result<Undo<N>, TryReserveError> {
        let needed = self
            .len()
            .checked_add(additional)
            .ok_or(TryReserveError::CapacityOverflow)?;
        let (held, capacity) = self
            .core
            .as_deref()
            .map_or((0, 0), |core| (core.chunks.len(), core.capacity));
        let mut undo = Undo {
            chunks: held,
            capacity,
            outgrown: None,
            created: false,
        };
        let (mut chunks, mut slots) = (held, capacity);
        while slots < needed {
            slots += Self::chunk_len(chunks);
            chunks += 1;
        }
        if chunks == held {
            return Ok(undo);
        }
        if self.core.is_none() {
            let core = Core {
                chunks: Vec::new(),
                len: 0,
                capacity: 0,
                free: NIL,
                free_count: 0,
            };
            self.core = Some(Box::try_new(core).map_err(|_| TryReserveError::AllocFailed)?);
            undo.created = true;
        }
        let grown = match self.core.as_deref_mut() {
            Some(core) => core.grow(held, chunks, slots),
            None => Err(TryReserveError::AllocFailed),
        };
        if grown.is_err() && undo.created {
            self.core = None;
        }
        grown.map(|outgrown| Undo { outgrown, ..undo })
    }

    /// Give back the room the reservation `undo` describes, before any node
    /// is pushed into it.
    fn undo(&mut self, undo: Undo<N>) {
        if undo.created {
            self.core = None;
            return;
        }
        let Some(core) = self.core.as_deref_mut() else {
            return;
        };
        core.chunks.truncate(undo.chunks);
        if let Some(mut list) = undo.outgrown {
            list.append(&mut core.chunks);
            core.chunks = list;
        }
        core.capacity = undo.capacity;
    }

    /// Allocate chunks onto `list`, whose room holds them, until it has
    /// `chunks`.
    fn fill(list: &mut Vec<Vec<N>>, chunks: usize) -> Result<(), TryReserveError> {
        while list.len() < chunks {
            let mut chunk = Vec::new();
            chunk
                .try_reserve_exact(Self::chunk_len(list.len()))
                .map_err(|_| TryReserveError::AllocFailed)?;
            list.push(chunk);
        }
        Ok(())
    }

    /// Make the next node empty in room already reserved, answering its
    /// index; `None` where no room was reserved: this never allocates.
    fn push_empty(&mut self) -> Option<usize>
    where
        N: Linked,
    {
        let core = self.core.as_deref_mut()?;
        let (chunk, _) = Self::locate(core.len);
        let held = core
            .chunks
            .get_mut(chunk)
            .filter(|held| held.len() < Self::chunk_len(chunk))?;
        N::write_empty(held.spare_capacity_mut().first_mut()?);
        // SAFETY: `write_empty` initialised the first spare slot, which lies
        // within the chunk's allocation, so the chunk holds one more node.
        unsafe { held.set_len(held.len() + 1) };
        core.len += 1;
        Some(core.len - 1)
    }

    /// Keep the first `len` nodes and give back every chunk past them, and
    /// any spare room in the list of chunks; a refusal of the fitted list
    /// changes nothing.
    fn fit(&mut self, len: usize) -> Result<(), TryReserveError> {
        let list = self.fitted_list(len)?;
        self.fit_with(len, list);
        Ok(())
    }

    /// The list of chunks a fit to `len` nodes moves them into: `None` where
    /// the list held has no spare room to give back.
    fn fitted_list(&self, len: usize) -> Result<Option<Vec<Vec<N>>>, TryReserveError> {
        let Some(core) = self.core.as_deref() else {
            return Ok(None);
        };
        let populated = Self::populated(len.min(core.len));
        // Fitted to nothing, the core goes and its list with it.
        if populated == 0 || core.chunks.capacity() <= populated {
            return Ok(None);
        }
        let mut fitted = Vec::new();
        fitted
            .try_reserve_exact(populated)
            .map_err(|_| TryReserveError::AllocFailed)?;
        Ok(Some(fitted))
    }

    /// [`Self::fit`], into the list [`Self::fitted_list`] made.
    fn fit_with(&mut self, len: usize, list: Option<Vec<Vec<N>>>) {
        let Some(core) = self.core.as_deref_mut() else {
            return;
        };
        let populated = Self::populated(len.min(core.len));
        while core.len > len {
            core.len -= 1;
            let (chunk, _) = Self::locate(core.len);
            if let Some(held) = core.chunks.get_mut(chunk) {
                held.pop();
            }
        }
        if populated == 0 {
            self.core = None;
            return;
        }
        core.chunks.truncate(populated);
        core.capacity = Self::slots_below(populated);
        if let Some(mut fitted) = list {
            fitted.append(&mut core.chunks);
            core.chunks = fitted;
        }
    }

    fn bytes(&self) -> usize {
        self.core.as_deref().map_or(0, |core| {
            core.capacity * mem::size_of::<N>()
                + core.chunks.capacity() * mem::size_of::<Vec<N>>()
                + mem::size_of::<Core<N>>()
        })
    }

    /// The first freed node chained, and how many are; none while it holds
    /// no core.
    fn free_chain(&self) -> (u32, u32) {
        self.core
            .as_deref()
            .map_or((NIL, 0), |core| (core.free, core.free_count))
    }

    /// Chain `count` freed nodes from `first`; a chain needs a node, so with
    /// no core there is none to set.
    fn set_free_chain(&mut self, first: u32, count: u32) {
        if let Some(core) = self.core.as_deref_mut() {
            (core.free, core.free_count) = (first, count);
        }
    }

    /// Every node, as one stretch a mutable walk opens from either end.
    pub(crate) fn block(&mut self) -> Block<'_, N> {
        let Some(core) = self.core.as_deref_mut() else {
            return Block::over(&mut [], 0);
        };
        let populated = Self::populated(core.len);
        let chunks = core.chunks.get_mut(..populated).unwrap_or_default();
        Block::over(chunks, core.len)
    }
}

impl<N> Core<N> {
    /// Grow from `held` chunks to `chunks` holding `slots`, answering the list
    /// of chunks it outgrew where it grew one; a refusal leaves it holding
    /// exactly the room it had.
    fn grow(
        &mut self,
        held: usize,
        chunks: usize,
        slots: usize,
    ) -> Result<Option<Vec<Vec<N>>>, TryReserveError> {
        if chunks <= self.chunks.capacity() {
            return match Nodes::fill(&mut self.chunks, chunks) {
                Ok(()) => {
                    self.capacity = slots;
                    Ok(None)
                }
                Err(refused) => {
                    self.chunks.truncate(held);
                    Err(refused)
                }
            };
        }
        // A list grown exactly while the chunks are small holds no spare
        // slot a small map would carry uncounted.
        let room = if chunks <= Nodes::<N>::SHIFT as usize + 1 {
            chunks
        } else {
            chunks.max(held * 2)
        };
        let mut grown = Vec::new();
        grown
            .try_reserve_exact(room)
            .map_err(|_| TryReserveError::AllocFailed)?;
        grown.append(&mut self.chunks);
        match Nodes::fill(&mut grown, chunks) {
            Ok(()) => {
                self.capacity = slots;
                Ok(Some(mem::replace(&mut self.chunks, grown)))
            }
            Err(refused) => {
                self.chunks.extend(grown.drain(..held));
                Err(refused)
            }
        }
    }
}

impl<N: Clone> Nodes<N> {
    fn try_clone(&self) -> Result<Self, TryReserveError> {
        let Some(core) = self.core.as_deref() else {
            return Ok(Self::new());
        };
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(core.chunks.len())
            .map_err(|_| TryReserveError::AllocFailed)?;
        for (number, held) in core.chunks.iter().enumerate() {
            let mut chunk = Vec::new();
            chunk
                .try_reserve_exact(Self::chunk_len(number))
                .map_err(|_| TryReserveError::AllocFailed)?;
            chunk.extend(held.iter().cloned());
            chunks.push(chunk);
        }
        let core = Core {
            chunks,
            len: core.len,
            capacity: core.capacity,
            free: core.free,
            free_count: core.free_count,
        };
        Ok(Self {
            core: Some(Box::try_new(core).map_err(|_| TryReserveError::AllocFailed)?),
        })
    }
}

/// A [`Nodes`]' chunks, borrowed: a read reaching many nodes pays for the
/// core's pointer once rather than at every node.
pub(crate) struct NodesView<'a, N> {
    chunks: &'a [Vec<N>],
}

impl<N> Clone for NodesView<'_, N> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<N> Copy for NodesView<'_, N> {}

impl<'a, N> NodesView<'a, N> {
    #[inline]
    pub(crate) fn get(self, index: usize) -> Option<&'a N> {
        let (chunk, at) = Nodes::<N>::locate(index);
        self.chunks.get(chunk)?.get(at)
    }
}

impl<N> Index<usize> for NodesView<'_, N> {
    type Output = N;

    fn index(&self, index: usize) -> &N {
        let (chunk, at) = Nodes::<N>::locate(index);
        &self.chunks[chunk][at]
    }
}

impl<N> Index<usize> for Nodes<N> {
    type Output = N;

    fn index(&self, index: usize) -> &N {
        let (chunk, at) = Self::locate(index);
        &self.view().chunks[chunk][at]
    }
}

impl<N> IndexMut<usize> for Nodes<N> {
    fn index_mut(&mut self, index: usize) -> &mut N {
        let (chunk, at) = Self::locate(index);
        let chunks: &mut [Vec<N>] = self
            .core
            .as_deref_mut()
            .map_or(&mut [], |core| &mut core.chunks);
        &mut chunks[chunk][at]
    }
}

/// Where a node of a [`Block`] lies.
#[derive(Clone, Copy)]
enum Place {
    Low(usize),
    Mid(usize, usize),
    High(usize),
}

/// A stretch of an arena's nodes, contiguous in index, borrowed whole so a
/// mutable walk can split nodes off either end and hold each it splits off
/// open while it goes on: the rest of the low end's chunk, the whole chunks
/// between, and the rest of the high end's chunk.
pub(crate) struct Block<'a, N> {
    low: &'a mut [N],
    /// Whole chunks, every one full, the first numbered `mid_first`.
    mid: &'a mut [Vec<N>],
    mid_first: usize,
    high: &'a mut [N],
    /// The arena index of `low`'s first node, the first unopened.
    base: usize,
    /// The arena index of `high`'s first node.
    high_base: usize,
    len: usize,
}

impl<'a, N> Block<'a, N> {
    fn over(chunks: &'a mut [Vec<N>], len: usize) -> Self {
        let (low, rest, mid_first): (&'a mut [N], &'a mut [Vec<N>], usize) = match chunks {
            [first, rest @ ..] => (first.as_mut_slice(), rest, 1),
            [] => (Default::default(), Default::default(), 0),
        };
        let (mid, high): (&'a mut [Vec<N>], &'a mut [N]) = match rest {
            [mid @ .., last] => (mid, last.as_mut_slice()),
            [] => (Default::default(), Default::default()),
        };
        let high_base = len.saturating_sub(high.len());
        Self {
            low,
            mid,
            mid_first,
            high,
            base: 0,
            high_base,
            len,
        }
    }

    /// A stretch holding nothing.
    pub(crate) fn empty() -> Self {
        Self::over(Default::default(), 0)
    }

    /// The arena index of the first unopened node.
    pub(crate) const fn base(&self) -> usize {
        self.base
    }

    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn first(&self) -> Option<&N> {
        self.low
            .first()
            .or_else(|| self.mid.first().and_then(|chunk| chunk.first()))
            .or_else(|| self.high.first())
    }

    pub(crate) fn last(&self) -> Option<&N> {
        self.high
            .last()
            .or_else(|| self.mid.last().and_then(|chunk| chunk.last()))
            .or_else(|| self.low.last())
    }

    /// Split the first node off, for the walk to hold.
    pub(crate) fn take_first(&mut self) -> Option<&'a mut N> {
        if self.low.is_empty() {
            self.low = match mem::take(&mut self.mid) {
                [first, rest @ ..] => {
                    self.mid = rest;
                    self.mid_first += 1;
                    first.as_mut_slice()
                }
                [] => mem::take(&mut self.high),
            };
        }
        let (node, rest) = mem::take(&mut self.low).split_first_mut()?;
        self.low = rest;
        self.base += 1;
        self.len -= 1;
        Some(node)
    }

    /// Split the last node off, for the walk to hold.
    pub(crate) fn take_last(&mut self) -> Option<&'a mut N> {
        if self.high.is_empty() {
            if let [rest @ .., last] = mem::take(&mut self.mid) {
                self.mid = rest;
                self.high_base = Nodes::<N>::slots_below(self.mid_first + self.mid.len());
                self.high = last.as_mut_slice();
            } else {
                self.high = mem::take(&mut self.low);
                self.high_base = self.base;
            }
        }
        let (node, rest) = mem::take(&mut self.high).split_last_mut()?;
        self.high = rest;
        self.len -= 1;
        Some(node)
    }

    /// Where the node `local` places past the first unopened one lies.
    fn place(&self, local: usize) -> Option<Place> {
        if local >= self.len {
            return None;
        }
        if local < self.low.len() {
            return Some(Place::Low(local));
        }
        let index = self.base + local;
        if let Some(at) = index
            .checked_sub(self.high_base)
            .filter(|&at| at < self.high.len())
        {
            return Some(Place::High(at));
        }
        let (chunk, at) = Nodes::<N>::locate(index);
        Some(Place::Mid(chunk.checked_sub(self.mid_first)?, at))
    }

    pub(crate) fn get(&self, local: usize) -> Option<&N> {
        match self.place(local)? {
            Place::Low(at) => self.low.get(at),
            Place::Mid(chunk, at) => self.mid.get(chunk)?.get(at),
            Place::High(at) => self.high.get(at),
        }
    }

    pub(crate) fn get_mut(&mut self, local: usize) -> Option<&mut N> {
        match self.place(local)? {
            Place::Low(at) => self.low.get_mut(at),
            Place::Mid(chunk, at) => self.mid.get_mut(chunk)?.get_mut(at),
            Place::High(at) => self.high.get_mut(at),
        }
    }

    pub(crate) fn swap(&mut self, first: usize, second: usize) {
        let (Some(one), Some(other)) = (self.place(first), self.place(second)) else {
            return;
        };
        let Self { low, mid, high, .. } = self;
        let pair = match (one, other) {
            (Place::Low(left), Place::Low(right)) => low.get_disjoint_mut([left, right]).ok(),
            (Place::High(left), Place::High(right)) => high.get_disjoint_mut([left, right]).ok(),
            (Place::Mid(chunk, left), Place::Mid(same, right)) if chunk == same => mid
                .get_mut(chunk)
                .and_then(|held| held.get_disjoint_mut([left, right]).ok()),
            (Place::Mid(chunk, left), Place::Mid(beyond, right)) => mid
                .get_disjoint_mut([chunk, beyond])
                .ok()
                .and_then(|[held, further]| held.get_mut(left).zip(further.get_mut(right)))
                .map(|(node, peer)| [node, peer]),
            (Place::Low(left), Place::Mid(chunk, right))
            | (Place::Mid(chunk, right), Place::Low(left)) => low
                .get_mut(left)
                .zip(mid.get_mut(chunk).and_then(|held| held.get_mut(right)))
                .map(|(node, peer)| [node, peer]),
            (Place::Low(left), Place::High(right)) | (Place::High(right), Place::Low(left)) => low
                .get_mut(left)
                .zip(high.get_mut(right))
                .map(|(node, peer)| [node, peer]),
            (Place::Mid(chunk, left), Place::High(right))
            | (Place::High(right), Place::Mid(chunk, left)) => mid
                .get_mut(chunk)
                .and_then(|held| held.get_mut(left))
                .zip(high.get_mut(right))
                .map(|(node, peer)| [node, peer]),
        };
        if let Some([node, peer]) = pair {
            mem::swap(node, peer);
        }
    }
}

/// A node arena and the chain of its freed slots, which lies in its nodes'
/// core, so an arena too costs its holder one word.
pub(crate) struct Arena<N> {
    pub(crate) nodes: Nodes<N>,
}

impl<N: Linked> Arena<N> {
    pub(crate) const fn new() -> Self {
        Self {
            nodes: Nodes::new(),
        }
    }

    /// How many freed nodes are chained.
    pub(crate) fn free_count(&self) -> usize {
        self.nodes.free_chain().1 as usize
    }

    /// Make room for `count` more nodes, freed ones counting, every one
    /// indexable by a `u32` below the links' sentinels.
    pub(crate) fn reserve(&mut self, count: usize) -> Result<(), TryReserveError> {
        self.make_room(count).map(drop)
    }

    /// [`Self::reserve`], answering what [`Nodes::undo`] needs to give the
    /// room back.
    fn make_room(&mut self, count: usize) -> Result<Undo<N>, TryReserveError> {
        let short = count.saturating_sub(self.free_count());
        if self.nodes.len().saturating_add(short) > FREE_END as usize {
            return Err(TryReserveError::CapacityOverflow);
        }
        self.nodes.reserve(short)
    }

    pub(crate) fn live(&self) -> usize {
        self.nodes.len() - self.free_count()
    }

    /// A node, freed first, else an empty one made in room reserved for it;
    /// [`NIL`] where no room was reserved.
    // A freed node is chained already empty, so taking one back resets
    // nothing but its link.
    pub(crate) fn take(&mut self) -> u32 {
        let (index, count) = self.nodes.free_chain();
        if index != NIL {
            if let Some(node) = self.nodes.get_mut(index as usize) {
                let next = match node.link() {
                    FREE_END => NIL,
                    next => next,
                };
                node.set_link(NIL);
                self.nodes.set_free_chain(next, count - 1);
                return index;
            }
        }
        self.nodes
            .push_empty()
            .and_then(|index| u32::try_from(index).ok())
            .unwrap_or(NIL)
    }

    /// Chain the empty node at `index` into the free list.
    pub(crate) fn free(&mut self, index: u32) {
        let (first, count) = self.nodes.free_chain();
        let link = if first == NIL { FREE_END } else { first };
        if let Some(node) = self.nodes.get_mut(index as usize) {
            node.set_link(link);
            self.nodes.set_free_chain(index, count + 1);
        }
    }

    /// Move the live node at `index`, where it lies at or past `live`, into
    /// the first freed slot below it from `hole` on, answering where it now
    /// is.
    pub(crate) fn relocate(&mut self, index: u32, live: usize, hole: &mut usize) -> u32 {
        if (index as usize) < live {
            return index;
        }
        while self.nodes.get(*hole).is_some_and(|node| node.link() == NIL) {
            *hole += 1;
        }
        self.nodes.swap(*hole, index as usize);
        let moved = u32::try_from(*hole).unwrap_or(NIL);
        *hole += 1;
        moved
    }

    /// Drop the `live`-onward nodes [`Self::relocate`] left freed, and give
    /// back the chunks they took; a refusal keeps them, chained free again.
    pub(crate) fn fit(&mut self, live: usize) -> Result<(), TryReserveError> {
        let fitted = self.nodes.fit(live);
        self.rechain(live);
        fitted
    }

    /// Start the free chain afresh after a fit: every node from `live` on
    /// that a refused one kept.
    fn rechain(&mut self, live: usize) {
        self.nodes.set_free_chain(NIL, 0);
        for index in (live..self.nodes.len()).rev() {
            let Ok(at) = u32::try_from(index) else {
                continue;
            };
            self.free(at);
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        self.nodes.bytes()
    }
}

/// Make room for `first_count` more nodes in `first` and `second_count` in
/// `second`, or for neither: a refusal leaves both holding exactly the room
/// they had.
pub(crate) fn reserve_both<A: Linked, B: Linked>(
    first: &mut Arena<A>,
    second: &mut Arena<B>,
    (first_count, second_count): (usize, usize),
) -> Result<(), TryReserveError> {
    let undo = first.make_room(first_count)?;
    match second.make_room(second_count) {
        Ok(_) => Ok(()),
        Err(refused) => {
            first.nodes.undo(undo);
            Err(refused)
        }
    }
}

/// Fit `first` and `second` to their `live` nodes as [`Arena::fit`] fits
/// one, or neither: a refused fitted list keeps both holding the room they
/// had, their freed nodes chained again.
pub(crate) fn fit_both<A: Linked, B: Linked>(
    first: &mut Arena<A>,
    second: &mut Arena<B>,
    (first_live, second_live): (usize, usize),
) -> Result<(), TryReserveError> {
    let lists = first.nodes.fitted_list(first_live).and_then(|one| {
        let two = second.nodes.fitted_list(second_live)?;
        Ok((one, two))
    });
    let fitted = lists.map(|(one, two)| {
        first.nodes.fit_with(first_live, one);
        second.nodes.fit_with(second_live, two);
    });
    first.rechain(first_live);
    second.rechain(second_live);
    fitted
}

impl<N: Clone> Arena<N> {
    /// A copy, node for node, its free chain with it.
    pub(crate) fn try_clone(&self) -> Result<Self, TryReserveError> {
        Ok(Self {
            nodes: self.nodes.try_clone()?,
        })
    }
}

#[cfg(test)]
#[path = "arena_tests.rs"]
mod tests;
