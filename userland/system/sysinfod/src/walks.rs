//! The lists walks are part way through, each held as the walk's first page
//! read it: a walk reads its source once, and sees one list however the
//! source changes before the walk ends.

use alloc::vec::Vec;

use tairix_abi::sysinfo::SysinfoQueryId;
use tairix_abi::ProcId;
use tairix_collections::LruMap;
use tairix_hash::BuildFastHash;
use tairix_inline::ArrayVec;

/// Walks one caller may have part way through at once; its oldest is let go
/// for another, so one process cannot take every other's.
pub const WALKS_PER_CALLER: usize = 4;

/// Bytes of usable RAM per byte of list held. The kernel spends more than
/// this many times a record's size on what the record names (a process's
/// stacks and tables), so a machine full of processes can still hold a walk
/// of every one.
const RAM_BYTES_PER_HELD_BYTE: u64 = 256;

/// Fewest bytes of list held, however little RAM there is: a small machine
/// still lists what lives on disk rather than in its RAM, its accounts.
const MIN_HELD_BYTES: usize = 256 << 10;

/// The most bytes of list this service holds for its walks, and reads whole
/// from a peer, on a machine with `total_ram_bytes` of usable RAM.
#[must_use]
pub fn list_budget(total_ram_bytes: u64) -> usize {
    usize::try_from(total_ram_bytes / RAM_BYTES_PER_HELD_BYTE)
        .unwrap_or(usize::MAX)
        .max(MIN_HELD_BYTES)
}

/// What each walk is charged beyond its list: the whole of the caller slot
/// it may occupy alone, so a flood of empty walks is bounded as one large
/// walk is.
const WALK_CHARGE: usize = size_of::<Own>();

/// One walk: whose, which, and of what.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WalkKey {
    /// The kernel-attested process walking.
    pub caller: ProcId,
    /// The walk, as the caller names it.
    pub walk: u32,
    /// The list walked.
    pub query: SysinfoQueryId,
}

/// One of a caller's walks: its list as its first page read it, or [`None`]
/// for one too large to hold, read afresh for each page.
struct Held {
    walk: u32,
    query: SysinfoQueryId,
    list: Option<Vec<u8>>,
    used: u64,
}

impl Held {
    fn is(&self, key: WalkKey) -> bool {
        self.walk == key.walk && self.query == key.query
    }

    fn charge(&self) -> usize {
        WALK_CHARGE + self.list.as_ref().map_or(0, Vec::len)
    }
}

/// A caller's walks.
type Own = ArrayVec<Held, WALKS_PER_CALLER>;

/// What a walk part way through holds of its list.
pub enum Walked<'a> {
    /// The list, encoded, as its first page read it.
    Held(&'a [u8]),
    /// Nothing: each page reads the list afresh.
    Unheld,
}

/// The lists walks are part way through, by caller, the least recently
/// active caller's oldest walk let go of first for room.
pub struct Walks {
    callers: LruMap<ProcId, Own, BuildFastHash>,
    charged: usize,
    budget: usize,
    tick: u64,
}

impl Walks {
    /// No walk part way through, on a machine with `total_ram_bytes` of
    /// usable RAM: what the walks may hold is sized from it.
    #[must_use]
    pub fn new(total_ram_bytes: u64) -> Self {
        Self {
            callers: LruMap::with_hasher(BuildFastHash::new()),
            charged: 0,
            budget: list_budget(total_ram_bytes),
            tick: 0,
        }
    }

    /// Begin `key`'s walk over `list`, its records encoded, in place of any
    /// walk it names already, letting go of older walks for room. A list
    /// larger than the walks may hold, or one [`None`] says could not be
    /// encoded, is read afresh for each page. A walk there is no memory to
    /// record at all is unknown, and its next page is answered
    /// [`tairix_abi::Errno::Interrupted`].
    pub fn begin(&mut self, key: WalkKey, list: Option<Vec<u8>>) {
        self.end(key);
        let list = list.filter(|list| WALK_CHARGE + list.len() <= self.budget);
        self.tick += 1;
        let held = Held {
            walk: key.walk,
            query: key.query,
            list,
            used: self.tick,
        };
        if self.callers.peek(&key.caller).is_some_and(Own::is_full) {
            self.let_go_oldest_of(key.caller);
        }
        while self.charged + held.charge() > self.budget {
            let Some((&caller, _)) = self.callers.peek_lru() else {
                break;
            };
            self.let_go_oldest_of(caller);
        }
        let charge = held.charge();
        let recorded = if let Some(own) = self.callers.get_mut(&key.caller) {
            own.try_push(held).is_ok()
        } else {
            let mut own = Own::new();
            own.try_push(held).is_ok() && self.callers.try_insert(key.caller, own).is_ok()
        };
        if recorded {
            self.charged += charge;
        }
    }

    /// What `key`'s walk holds, [`None`] for a walk unknown.
    pub fn list(&mut self, key: WalkKey) -> Option<Walked<'_>> {
        let at = self
            .callers
            .peek(&key.caller)?
            .iter()
            .position(|held| held.is(key))?;
        self.tick += 1;
        let held = self.callers.get_mut(&key.caller)?.get_mut(at)?;
        held.used = self.tick;
        Some(match &held.list {
            Some(list) => Walked::Held(list),
            None => Walked::Unheld,
        })
    }

    /// End `key`'s walk, letting go of its list.
    pub fn end(&mut self, key: WalkKey) {
        self.let_go(key.caller, |own| own.iter().position(|held| held.is(key)));
    }

    /// Let go of `caller`'s least recently used walk.
    fn let_go_oldest_of(&mut self, caller: ProcId) {
        self.let_go(caller, |own| {
            own.iter()
                .enumerate()
                .min_by_key(|(_, held)| held.used)
                .map(|(at, _)| at)
        });
    }

    /// Let go of the walk of `caller`'s that `pick` finds, and of the caller
    /// once it has none.
    fn let_go(&mut self, caller: ProcId, pick: impl FnOnce(&Own) -> Option<usize>) {
        let Some(own) = self.callers.peek_mut(&caller) else {
            return;
        };
        let Some(held) = pick(own).and_then(|at| own.swap_remove(at)) else {
            return;
        };
        if own.is_empty() {
            self.callers.remove(&caller);
        }
        self.charged -= held.charge();
    }
}

#[cfg(test)]
#[path = "walks_tests.rs"]
mod tests;
