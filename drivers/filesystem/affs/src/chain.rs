//! The extension chains of the files most recently reached, kept across
//! changes: a growth adds the blocks it links and a cut drops those it frees,
//! so a held file's chain is walked from its header once.
//!
//! A held chain is always a prefix of its file's chain as the medium holds
//! it. One walked on to the block that ends it names no block twice: a chain
//! that returned to a block it had passed would repeat from there and never
//! end.

use alloc::vec::Vec;

use tairix_abi::DriverError;

use crate::guard::Guard;

/// The chains held at most; past it, the least recently reached is dropped.
const CHAINS_KEPT: usize = 8;

/// One file's extension blocks from its header on, as far as they are known.
struct Chain {
    file: u64,
    blocks: Vec<u32>,
    /// The successor the last block records, read with it but not yet walked
    /// to; `Some(0)` where the chain is known to end there.
    next: Option<u32>,
    guard: Guard,
}

impl Chain {
    fn new(file: u64, volume_blocks: u64) -> Self {
        Self {
            file,
            blocks: Vec::new(),
            next: None,
            guard: Guard::new(file, volume_blocks),
        }
    }

    /// The chain cut to its first `len` blocks, which the medium now ends
    /// there, giving back room a much longer chain held.
    fn end_at(&mut self, len: usize, volume_blocks: u64) {
        self.blocks.truncate(len);
        self.next = Some(0);
        self.guard = Guard::new(self.file, volume_blocks);
        if self.blocks.len() < self.blocks.capacity() / 4 {
            tairix_util::fallible::trim(&mut self.blocks);
        }
    }
}

/// The chains held, the most recently reached first.
///
/// Together they hold at most `room` blocks, the chain of a file of the
/// largest size the format records, and a cut gives back what it no longer
/// needs, so their allocations stay within four times that. The default
/// holds none and has no room to.
#[derive(Default)]
pub(crate) struct Chains {
    held: Vec<Chain>,
    room: usize,
}

impl Chains {
    /// Chains of at most `room` blocks between them.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`].
    pub(crate) fn new(room: usize) -> Result<Self, DriverError> {
        let mut held = Vec::new();
        held.try_reserve_exact(CHAINS_KEPT)
            .map_err(|_| DriverError::OutOfMemory)?;
        Ok(Self { held, room })
    }

    /// Extension block `number` of the file whose header is `file` and links
    /// `head`, walked on from as far as it is held, `next_of` reading the
    /// successor a block records.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for a chain that ends short of `number`,
    /// loops, or runs on past any file's; `next_of`'s error;
    /// [`DriverError::OutOfMemory`], the chain then forgotten.
    pub(crate) fn block(
        &mut self,
        file: u64,
        head: u32,
        number: u64,
        volume_blocks: u64,
        mut next_of: impl FnMut(u32) -> Result<u32, DriverError>,
    ) -> Result<u32, DriverError> {
        let at = usize::try_from(number).map_err(|_| DriverError::BadMagic)?;
        self.reach(file, volume_blocks)?;
        loop {
            if let Some(&block) = self.held.first().and_then(|chain| chain.blocks.get(at)) {
                return Ok(block);
            }
            if self.extend(head, &mut next_of)? == 0 {
                return Err(DriverError::BadMagic);
            }
        }
    }

    /// Walk the chain of the file whose header is `file` and links `head` on
    /// to the block that ends it.
    ///
    /// # Errors
    ///
    /// As [`Self::block`]'s, for a chain that does not end.
    pub(crate) fn end(
        &mut self,
        file: u64,
        head: u32,
        volume_blocks: u64,
        mut next_of: impl FnMut(u32) -> Result<u32, DriverError>,
    ) -> Result<(), DriverError> {
        if head == 0 {
            return Ok(());
        }
        self.reach(file, volume_blocks)?;
        while self.extend(head, &mut next_of)? != 0 {}
        Ok(())
    }

    /// The front chain one block longer, answering the block it reached, or
    /// 0 where the chain ends.
    fn extend(
        &mut self,
        head: u32,
        next_of: &mut impl FnMut(u32) -> Result<u32, DriverError>,
    ) -> Result<u32, DriverError> {
        let room = self.room;
        let chain = self.held.first_mut().ok_or(DriverError::OutOfMemory)?;
        let next = match (chain.blocks.last(), chain.next) {
            (None, _) => head,
            (Some(_), Some(next)) => next,
            (Some(&last), None) => next_of(last)?,
        };
        if next == 0 {
            chain.next = Some(0);
            return Ok(0);
        }
        // No file of the largest size has more extension blocks.
        if chain.blocks.len() >= room {
            return Err(DriverError::BadMagic);
        }
        chain.guard.step(u64::from(next))?;
        chain.next = None;
        if !self.push(next) {
            self.held.remove(0);
            return Err(DriverError::OutOfMemory);
        }
        Ok(next)
    }

    /// Extension block `number` of `file` was read, recording `next` as its
    /// successor: kept where it is the last block held.
    pub(crate) fn learned(&mut self, file: u64, number: u64, next: u32) {
        if let Some(chain) = self.held.iter_mut().find(|chain| chain.file == file) {
            if usize::try_from(number).is_ok_and(|at| at + 1 == chain.blocks.len()) {
                chain.next = Some(next);
            }
        }
    }

    /// `block`, just linked as extension block `number` of `file`, now ends
    /// its chain. A chain not held that far is left unheld, and one with no
    /// room for the block is forgotten: the next walk starts again from the
    /// header, so a refusal never fails the change that linked it.
    pub(crate) fn linked(&mut self, file: u64, number: u64, block: u32, volume_blocks: u64) {
        let Ok(len) = usize::try_from(number) else {
            self.forget(file);
            return;
        };
        if len == 0 {
            if self.reach(file, volume_blocks).is_err() {
                return;
            }
        } else if self.held.first().is_none_or(|chain| chain.file != file) {
            let Some(at) = self.position(file) else {
                return;
            };
            self.held[..=at].rotate_right(1);
        }
        let Some(chain) = self.held.first_mut() else {
            return;
        };
        if chain.blocks.len() < len {
            self.held.remove(0);
            return;
        }
        chain.end_at(len, volume_blocks);
        if !self.push(block) {
            self.held.remove(0);
        }
    }

    /// `file`'s chain was cut to its first `keep` blocks.
    pub(crate) fn cut(&mut self, file: u64, keep: u64, volume_blocks: u64) {
        let Some(at) = self.position(file) else {
            return;
        };
        match usize::try_from(keep) {
            Ok(keep) if keep > 0 && keep <= self.held[at].blocks.len() => {
                self.held[at].end_at(keep, volume_blocks);
            }
            _ => {
                self.held.remove(at);
            }
        }
    }

    /// Drop `file`'s chain: its header is freed, and may be taken again.
    pub(crate) fn forget(&mut self, file: u64) {
        if let Some(at) = self.position(file) {
            self.held.remove(at);
        }
    }

    /// Drop every chain: a change stopped part-way may have linked or cut one
    /// on the medium alone.
    pub(crate) fn clear(&mut self) {
        self.held.clear();
    }

    fn position(&self, file: u64) -> Option<usize> {
        self.held.iter().position(|chain| chain.file == file)
    }

    /// Put `file`'s chain at the front, an empty one for a file not held,
    /// dropping the least recently reached to make room.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`] where no chain can be held.
    fn reach(&mut self, file: u64, volume_blocks: u64) -> Result<(), DriverError> {
        if let Some(at) = self.position(file) {
            self.held[..=at].rotate_right(1);
            return Ok(());
        }
        let kept = CHAINS_KEPT.min(self.held.capacity());
        if kept == 0 {
            return Err(DriverError::OutOfMemory);
        }
        self.held.truncate(kept - 1);
        self.held.insert(0, Chain::new(file, volume_blocks));
        Ok(())
    }

    /// The blocks held of `file`'s chain, if it is held.
    #[cfg(test)]
    pub(crate) fn held(&self, file: u64) -> Option<Vec<u32>> {
        self.position(file).map(|at| self.held[at].blocks.clone())
    }

    /// Append `block` to the front chain, first dropping the least recently
    /// reached others while the chains hold their room between them.
    fn push(&mut self, block: u32) -> bool {
        while self.held.len() > 1
            && self
                .held
                .iter()
                .map(|chain| chain.blocks.len())
                .sum::<usize>()
                >= self.room
        {
            self.held.pop();
        }
        self.held
            .first_mut()
            .is_some_and(|chain| tairix_util::fallible::push(&mut chain.blocks, block))
    }
}

#[cfg(test)]
mod tests {
    use tairix_abi::DriverError;

    use super::{Chains, CHAINS_KEPT};

    const VOLUME: u64 = 1 << 20;

    /// A walk over a chain that links block `n` to `n + 1` up to `last`,
    /// counting the reads it makes.
    fn walk(chains: &mut Chains, file: u64, number: u64, last: u32, reads: &mut usize) -> u32 {
        chains
            .block(file, 1000, number, VOLUME, |block| {
                *reads += 1;
                Ok(if block >= last { 0 } else { block + 1 })
            })
            .expect("a block of the chain")
    }

    #[test]
    fn a_walk_reads_each_block_once_and_resumes_where_it_stopped() {
        let mut chains = Chains::new(1000).expect("room");
        let mut reads = 0;
        assert_eq!(walk(&mut chains, 7, 3, 1100, &mut reads), 1003);
        assert_eq!(reads, 3, "the header's link and three successors");
        assert_eq!(walk(&mut chains, 7, 2, 1100, &mut reads), 1002);
        assert_eq!(walk(&mut chains, 7, 5, 1100, &mut reads), 1005);
        assert_eq!(reads, 5);
        chains.learned(7, 5, 1006);
        assert_eq!(walk(&mut chains, 7, 6, 1100, &mut reads), 1006);
        assert_eq!(reads, 5, "a successor learned is not read again");
    }

    #[test]
    fn the_least_recently_reached_chain_is_given_up_and_the_room_is_shared() {
        let mut chains = Chains::new(1000).expect("room");
        let mut reads = 0;
        for file in 0..=CHAINS_KEPT as u64 {
            walk(&mut chains, file, 0, 1100, &mut reads);
            if file == 2 {
                // Reached again, so it outlives files 0 and 1.
                walk(&mut chains, 0, 0, 1100, &mut reads);
            }
        }
        assert_eq!(chains.held.len(), CHAINS_KEPT);
        assert!(chains.held(1).is_none(), "the least recently reached");
        assert!(chains.held(0).is_some());

        let mut shared = Chains::new(10).expect("room");
        walk(&mut shared, 1, 5, 1100, &mut reads);
        walk(&mut shared, 2, 5, 1100, &mut reads);
        assert!(shared.held(1).is_none(), "dropped for the chain growing");
        assert_eq!(shared.held(2).map(|blocks| blocks.len()), Some(6));
    }

    #[test]
    fn a_walk_past_the_chain_or_the_room_or_round_a_loop_is_damage() {
        let mut chains = Chains::new(1000).expect("room");
        let mut reads = 0;
        walk(&mut chains, 1, 0, 1003, &mut reads);
        assert_eq!(
            chains.block(1, 1000, 4, VOLUME, |block| Ok(if block >= 1003 {
                0
            } else {
                block + 1
            })),
            Err(DriverError::BadMagic),
            "the chain ends at its fourth block"
        );
        let mut small = Chains::new(3).expect("room");
        assert_eq!(
            small.block(1, 1000, 3, VOLUME, |block| Ok(block + 1)),
            Err(DriverError::BadMagic),
            "more blocks than any file has"
        );
        let mut looped = Chains::new(1000).expect("room");
        assert_eq!(
            looped.end(1, 1000, VOLUME, |block| Ok(if block == 1002 {
                1000
            } else {
                block + 1
            })),
            Err(DriverError::BadMagic),
            "a chain that returns to its first block never ends"
        );
    }

    #[test]
    fn a_link_ends_the_chain_and_a_cut_shortens_it() {
        let mut chains = Chains::new(1000).expect("room");
        let mut reads = 0;
        walk(&mut chains, 1, 3, 1100, &mut reads);
        chains.linked(1, 2, 77, VOLUME);
        assert_eq!(chains.held(1), Some(alloc::vec![1000, 1001, 77]));
        assert_eq!(
            chains.end(1, 1000, VOLUME, |_| panic!(
                "a linked chain is known to end"
            )),
            Ok(())
        );
        chains.linked(1, 0, 5, VOLUME);
        assert_eq!(chains.held(1), Some(alloc::vec![5]));
        chains.linked(9, 3, 6, VOLUME);
        assert!(
            chains.held(9).is_none(),
            "an unheld chain is not started past its head"
        );
        chains.linked(1, 4, 6, VOLUME);
        assert!(
            chains.held(1).is_none(),
            "a chain not held that far is dropped"
        );

        walk(&mut chains, 2, 4, 1100, &mut reads);
        chains.learned(2, 4, 1005);
        chains.cut(2, 2, VOLUME);
        assert_eq!(chains.held(2), Some(alloc::vec![1000, 1001]));
        assert_eq!(
            chains.end(2, 1000, VOLUME, |_| panic!("a cut chain is known to end")),
            Ok(())
        );
        chains.learned(2, 0, 1001);
        assert_eq!(
            chains.held(2).map(|blocks| blocks.len()),
            Some(2),
            "only the last learns"
        );
        chains.cut(2, 0, VOLUME);
        assert!(chains.held(2).is_none());
        walk(&mut chains, 3, 1, 1100, &mut reads);
        chains.forget(3);
        assert!(chains.held(3).is_none());
        walk(&mut chains, 4, 1, 1100, &mut reads);
        chains.clear();
        assert!(chains.held.is_empty());
    }

    #[test]
    fn a_cut_chain_gives_its_room_back() {
        let room = 1000;
        let mut chains = Chains::new(room).expect("room");
        for file in 0..CHAINS_KEPT as u64 {
            chains.linked(file, 0, 1, VOLUME);
            for number in 1..800u32 {
                chains.linked(file, u64::from(number), 1 + number, VOLUME);
            }
            chains.cut(file, 1, VOLUME);
        }
        let capacity: usize = chains
            .held
            .iter()
            .map(|chain| chain.blocks.capacity())
            .sum();
        assert_eq!(chains.held.len(), CHAINS_KEPT);
        assert!(capacity <= 2 * room, "{capacity} blocks' room held");
    }

    #[test]
    fn a_link_with_no_room_forgets_the_chain_and_fails_nothing() {
        let mut chains = Chains::new(100_000).expect("room");
        chains.linked(1, 0, 1, VOLUME);
        let mut number = 1u32;
        while chains.held[0].blocks.len() < chains.held[0].blocks.capacity() {
            chains.linked(1, u64::from(number), 1 + number, VOLUME);
            number += 1;
        }
        let capacity = chains.held[0].blocks.capacity();
        tairix_fuzzseed::meter::refusing_above(capacity * 4, || {
            chains.linked(1, u64::from(number), 1 + number, VOLUME);
        });
        assert!(chains.held(1).is_none());
    }
}
