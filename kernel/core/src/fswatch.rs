//! Filesystem change notification (`docs/src/filesystem/watch.md`).
//!
//! Two sources share one per-volume table keyed by node:
//!
//! * the **change generation** a [`WaitSourceKind::File`] member is edge-
//!   triggered on (`tail -f`), and
//! * the **change journal** of a directory armed with `fs_watch`: the names
//!   whose entries changed, coalesced so a name rewritten ten thousand times
//!   is one record, shared by every watcher of the directory, each holding its
//!   own cursor. Every journal together holds at most a share of the
//!   machine's memory, each an equal part of it and never past
//!   [`JOURNAL_BYTES`], so a watcher that never drains crowds out no other.
//!
//! The tables, the watches each process holds, the heap every journal holds
//! together and the epochs a watched path resolves against live in one
//! [`WatchRegistry`] per kernel, handed to the
//! syscall layer and to the volume bring-up that claims tables. A table is
//! fed by the one cache wrapper ([`super::fs::CachedFs`]) that
//! [claims](WatchRegistry::claim) it, through which every mutation of the volume passes
//! under the volume lock, so the record order is the mutation order and no
//! writer is missed. The table outlives the claim: a volume that leaves and
//! returns is watched through the same table, so its watchers carry on.
//! A journal that cannot name what happened — it overflowed, memory is short,
//! the volume folds case, a directory's authority changed — says *rescan*
//! instead. That is never a loss: the watcher re-reads the directory.
//!
//! # Lock order
//!
//! A table's lock is taken under the volume lock (a mutation), an address
//! space's lock (a descriptor closing) or the registry's table lock (sharing
//! out the journal budget); under it only the heap and the registry's epoch
//! lock are taken. A wake is issued once the table lock is released, directly,
//! except a mount change's, which is deferred because the mount table's lock
//! is held.
//!
//! [`WaitSourceKind::File`]: tairix_abi::WaitSourceKind::File

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::borrow::Borrow;
use core::hash::{Hash, Hasher};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use tairix_abi::driver::filesystem::{NameMatching, NodeId};
use tairix_abi::{DirChange, Errno, FileId};
use tairix_collections::{HashMap, LruMap};
use tairix_hash::BuildSipHash13;
use tairix_kernel_sec::ProcessId;
use tairix_reclaim::{shrink_target, CacheBudget, MemoryPressure, ReclaimClass};
use tairix_sync::SpinLock;
use tairix_util::secret::WipedBuf;
use zeroize::Zeroize;

use crate::fs::ChildMounts;
use crate::waitq::WakeKey;

/// The most one directory's journal holds, estimated as its name bytes plus
/// `ENTRY_COST` per name. A containment bound, not a capacity: past it the
/// watchers rescan, which for a change that large is the cheaper read anyway.
pub const JOURNAL_BYTES: usize = 32 * 1024;

/// What one recorded name costs beyond its bytes: its map slot, recency links
/// and index, the name's allocation header, and the map's growth slack.
const ENTRY_COST: usize = 128;

/// Every journal together holds at most one part in this many of the
/// machine's memory, less as pressure rises; past it a journal that would
/// grow rescans instead.
const JOURNAL_SHARE: usize = 512;

/// A recorded name. File names are decrypted user data, so the bytes are
/// zeroed before the allocation is released.
struct Name(Box<[u8]>);

impl Drop for Name {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl Borrow<[u8]> for Name {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

impl Hash for Name {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // `[u8]`'s own hash, so a lookup by `&[u8]` finds the entry.
        (*self.0).hash(state);
    }
}

impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Name {}

fn name_cost(len: usize) -> usize {
    len + ENTRY_COST
}

fn copy_name(name: &[u8]) -> Option<Name> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(name.len()).ok()?;
    bytes.extend_from_slice(name);
    Some(Name(bytes.into_boxed_slice()))
}

/// Where one watcher stands in its directory's journal.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Position {
    /// Every change at or below this has been delivered.
    pub at: u64,
    /// What the watcher's path last resolved under.
    pub epochs: Epochs,
}

/// A directory's change journal.
///
/// The recency order of `names` is the order of their `seq`, so the changes a
/// cursor has not taken are always a suffix of it.
struct Journal {
    /// Advances with every recorded change and every revalidation.
    seq: u64,
    /// A cursor below this missed a change the journal no longer names.
    floor: u64,
    /// The highest position any cursor holds or any drain has taken. A change
    /// recorded above it is pending for every watcher, and whichever drain
    /// takes it reads the entry after now — so recording it again adds
    /// nothing.
    taken: u64,
    names: LruMap<Name, u64, BuildSipHash13>,
    /// The estimate a journal's share of the budget bounds.
    bytes: usize,
    registry: &'static WatchRegistry,
    /// The heap this journal last counted into the registry's.
    counted: usize,
    /// Each watcher's position, by watcher.
    cursors: HashMap<u64, Position, BuildSipHash13>,
    /// The lowest position a cursor holds, and how many hold it: what every
    /// watcher has taken is found without visiting each cursor.
    oldest: u64,
    at_oldest: usize,
}

/// What [`Journal::note`] did.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Noted {
    /// Already pending for every watcher; nothing moved.
    Covered,
    /// The journal advanced.
    Advanced,
}

impl Journal {
    fn new(registry: &'static WatchRegistry) -> Self {
        registry.journal_count.fetch_add(1, Ordering::Relaxed);
        Self {
            seq: 0,
            floor: 0,
            taken: 0,
            names: LruMap::with_hasher(hasher()),
            bytes: 0,
            registry,
            counted: 0,
            cursors: HashMap::with_hasher(hasher()),
            oldest: 0,
            at_oldest: 0,
        }
    }

    /// The heap the journal's record holds: the map and every name it owns —
    /// `bytes` less the per-name estimate. The cursors are each watch's own,
    /// bounded with the watch by the owner's limit rather than shared out here.
    fn footprint(&self) -> usize {
        let names = self
            .bytes
            .saturating_sub(self.names.len().saturating_mul(ENTRY_COST));
        self.names.allocated_bytes() + names
    }

    /// Bring the registry's count into line with what the journal holds now.
    fn recount(&mut self) {
        let now = self.footprint();
        let live = &self.registry.journal_bytes;
        if now >= self.counted {
            live.fetch_add(now - self.counted, Ordering::Relaxed);
        } else {
            live.fetch_sub(self.counted - now, Ordering::Relaxed);
        }
        self.counted = now;
    }

    /// Record that `name`'s entry changed. `budget` is the heap every journal
    /// together may hold, `share` the most this one may.
    fn note(&mut self, name: &[u8], precise: bool, (budget, share): (usize, usize)) -> Noted {
        let pending = self.names.peek(name).map_or(self.floor, |&seq| seq);
        if pending > self.taken {
            return Noted::Covered;
        }
        self.seq += 1;
        if !precise {
            self.lose_names();
            return Noted::Advanced;
        }
        if let Some(seq) = self.names.get_mut(name) {
            *seq = self.seq;
            return Noted::Advanced;
        }
        let cost = name_cost(name.len());
        if cost > share || self.registry.journal_bytes.load(Ordering::Relaxed) >= budget {
            self.lose_names();
            return Noted::Advanced;
        }
        self.shed_to(share - cost);
        let Some(copy) = copy_name(name) else {
            self.lose_names();
            return Noted::Advanced;
        };
        if self.names.try_insert(copy, self.seq).is_err() {
            self.lose_names();
            return Noted::Advanced;
        }
        self.bytes += cost;
        self.recount();
        Noted::Advanced
    }

    /// Drop the oldest names until what is recorded fits `bytes`; a watcher
    /// still owed one rescans. A map left at half what it grew to is rebuilt
    /// at what it holds, so the heap goes back with the names.
    fn shed_to(&mut self, bytes: usize) {
        let before = self.bytes;
        while self.bytes > bytes {
            let Some((evicted, seq)) = self.names.pop_lru() else {
                break;
            };
            self.bytes -= name_cost(evicted.0.len());
            self.floor = self.floor.max(seq);
        }
        if self.bytes == before {
            return;
        }
        if self.names.len() <= self.names.capacity() / 2 {
            self.compact();
        }
        self.recount();
    }

    /// Rebuild the map at what it holds, keeping its recency order.
    fn compact(&mut self) {
        let Ok(mut kept) =
            LruMap::try_with_capacity_and_hasher(self.names.len(), *self.names.hasher())
        else {
            return;
        };
        while let Some((name, seq)) = self.names.pop_lru() {
            let cost = name_cost(name.0.len());
            // The room was reserved; a name refused anyway is one its
            // cursors rescan for.
            if kept.try_insert(name, seq).is_err() {
                self.bytes -= cost;
                self.floor = self.floor.max(seq);
            }
        }
        self.names = kept;
    }

    /// Every watcher rescans: what the journal held is no longer enough. The
    /// map's own allocations go with the names.
    fn lose_names(&mut self) {
        self.floor = self.seq;
        self.names.clear();
        self.bytes = 0;
        self.recount();
    }

    /// A change no name expresses: the directory moved, or what its path
    /// reaches may have.
    fn revalidate(&mut self) {
        self.seq += 1;
        self.lose_names();
    }

    fn cursor(&self, watcher: u64) -> Option<Position> {
        self.cursors.get(&watcher).copied()
    }

    /// Start `watcher` where the journal stands, its path having resolved
    /// under `epochs`.
    fn join(&mut self, watcher: u64, epochs: Epochs) -> Result<(), Errno> {
        let at = self.seq;
        self.cursors
            .try_insert(watcher, Position { at, epochs })
            .map_err(|_| Errno::OutOfMemory)?;
        // No cursor stands past the journal, so the new one is the oldest only
        // where every other stands with it.
        if self.cursors.len() == 1 {
            (self.oldest, self.at_oldest) = (at, 1);
        } else if at == self.oldest {
            self.at_oldest += 1;
        }
        self.taken = self.taken.max(at);
        self.recount();
        Ok(())
    }

    /// Drop `watcher`'s cursor; `true` once no watcher is left.
    fn leave(&mut self, watcher: u64) -> bool {
        if let Some(left) = self.cursors.remove(&watcher) {
            self.passed(left.at);
        }
        if self.cursors.is_empty() {
            return true;
        }
        self.collect();
        false
    }

    /// A cursor left position `at`: once none holds the oldest, find the next.
    fn passed(&mut self, at: u64) {
        if at != self.oldest {
            return;
        }
        self.at_oldest = self.at_oldest.saturating_sub(1);
        if self.at_oldest == 0 {
            let oldest = self
                .cursors
                .values()
                .map(|c| c.at)
                .min()
                .unwrap_or(self.seq);
            self.oldest = oldest;
            self.at_oldest = self.cursors.values().filter(|c| c.at == oldest).count();
        }
    }

    /// What `watcher` has not taken, its records sized to `budget` bytes, or
    /// a rescan when `rescan` says what it holds is no longer the watcher's
    /// to read.
    fn take(&mut self, watcher: u64, budget: usize, rescan: bool) -> Result<Option<Drain>, Errno> {
        let Some(Position { at: cursor, .. }) = self.cursor(watcher) else {
            return Ok(None);
        };
        if rescan || cursor < self.floor {
            self.taken = self.taken.max(self.seq);
            return Ok(Some(Drain::Rescan { upto: self.seq }));
        }
        let pending = self
            .names
            .iter_lru()
            .rev()
            .take_while(|(_, &seq)| seq > cursor)
            .count();
        let start = self.names.len() - pending;
        let (mut count, mut bytes, mut spent, mut upto) = (0, 0, 0, cursor);
        for (name, &seq) in self.names.iter_lru().skip(start) {
            let cost = DirChange::len_for(true, name.0.len());
            if spent + cost > budget {
                break;
            }
            spent += cost;
            count += 1;
            bytes += name.0.len();
            upto = seq;
        }
        let more = count < pending;
        if !more {
            upto = self.seq;
        }
        let names = ChangedNames::with_exact(
            self.names
                .iter_lru()
                .skip(start)
                .take(count)
                .map(|(name, _)| &*name.0),
            count,
            bytes,
        )?;
        self.taken = self.taken.max(upto);
        Ok(Some(Drain::Names { names, upto, more }))
    }

    fn commit(&mut self, watcher: u64, upto: u64, epochs: Epochs) {
        let Some(cursor) = self.cursors.get_mut(&watcher) else {
            return;
        };
        let was = cursor.at;
        cursor.at = was.max(upto);
        cursor.epochs = epochs;
        if cursor.at != was {
            self.passed(was);
        }
        self.collect();
    }

    /// Drop the names every watcher has taken; a journal left empty releases
    /// its map's storage, so a burst does not stay resident.
    fn collect(&mut self) {
        let oldest = if self.cursors.is_empty() {
            self.seq
        } else {
            self.oldest
        };
        while let Some((_, &seq)) = self.names.peek_lru() {
            if seq > oldest {
                break;
            }
            if let Some((name, _)) = self.names.pop_lru() {
                self.bytes -= name_cost(name.0.len());
            }
        }
        if self.names.is_empty() {
            self.names.clear();
        }
        self.recount();
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        self.registry
            .journal_bytes
            .fetch_sub(self.counted, Ordering::Relaxed);
        self.registry.journal_count.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The names a drain took, in the order they were recorded, in one exactly
/// sized allocation, wiped before it is released as a recorded name is.
pub struct ChangedNames {
    bytes: WipedBuf,
    ends: Vec<usize>,
}

impl ChangedNames {
    /// `count` names totalling `bytes`, allocated once.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] if the allocation is refused.
    fn with_exact<'a>(
        names: impl Iterator<Item = &'a [u8]>,
        count: usize,
        bytes: usize,
    ) -> Result<Self, Errno> {
        let mut joined = Vec::new();
        joined
            .try_reserve_exact(bytes)
            .map_err(|_| Errno::OutOfMemory)?;
        let mut ends = Vec::new();
        ends.try_reserve_exact(count)
            .map_err(|_| Errno::OutOfMemory)?;
        for name in names.take(count) {
            joined.extend_from_slice(name);
            ends.push(joined.len());
        }
        Ok(Self {
            bytes: WipedBuf::new(joined),
            ends,
        })
    }

    /// How many names were taken.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Whether no name was taken.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// The names, in recorded order.
    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        let mut start = 0;
        self.ends.iter().map(move |&end| {
            let name = self.bytes.get(start..end).unwrap_or_default();
            start = end;
            name
        })
    }
}

/// What a watcher's drain takes from its journal.
pub enum Drain {
    /// These names changed.
    Names {
        /// The changed names, in the order they were recorded.
        names: ChangedNames,
        /// The journal position delivering them advances the cursor to.
        upto: u64,
        /// Further changes are recorded beyond `upto`.
        more: bool,
    },
    /// The watcher missed what the journal no longer names; a re-read of the
    /// directory takes over.
    Rescan {
        /// The journal position the re-read takes over from.
        upto: u64,
    },
}

impl Drain {
    /// The journal position delivering this drain advances the cursor to.
    #[must_use]
    pub const fn upto(&self) -> u64 {
        match self {
            Self::Names { upto, .. } | Self::Rescan { upto } => *upto,
        }
    }
}

/// The epochs a watched path resolves against. Any of them moving may change
/// what the path reaches or who may list it. The path and access epochs are
/// the whole machine's, because a path's ancestors need not be on the volume
/// its directory is.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Epochs {
    /// The mount table's epoch.
    pub mounts: u64,
    /// Moves when any directory or symbolic link is renamed, removed or
    /// replaced.
    pub paths: u64,
    /// Moves when any directory's security changes.
    pub access: u64,
}

/// What a watch's path last resolved under: its epochs, and the mounts its
/// directory's listing showed directly beneath it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Resolved {
    /// The epochs.
    pub epochs: Epochs,
    /// The mounts directly beneath the directory.
    pub children: ChildMounts,
}

/// What a [`WaitSourceKind::DirWatch`](tairix_abi::WaitSourceKind::DirWatch)
/// member last reported, and how it paces the next report.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Pacing {
    /// The arming of the watch the member observes; a descriptor closed and
    /// armed again is another watch, which the member never reports.
    pub watcher: u64,
    /// The journal position the member last reported.
    pub observed: u64,
    /// The epochs the member last reported under.
    pub epochs: Epochs,
    /// The least interval between two reports, from the watch.
    pub latency_ns: u64,
    /// When the member last reported, in monotonic nanoseconds; [`None`]
    /// until it first does, so its first change is never held.
    pub reported_at: Option<u64>,
}

/// How a [`WaitSourceKind::DirWatch`](tairix_abi::WaitSourceKind::DirWatch)
/// member stands at a scan.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MemberState {
    /// Report it now.
    Ready,
    /// A change is held until this monotonic instant, the watch's latency
    /// after the previous report.
    Due(u64),
    /// Nothing since the last report.
    Idle,
}

struct NodeWatch {
    key: WakeKey,
    /// Bumped by every change a File member reports on.
    generation: u64,
    file_members: u32,
    /// A waiter registered under `key` has nothing pending; the next change
    /// it reports wakes the key and clears this.
    parked: bool,
    journal: Option<Journal>,
}

impl NodeWatch {
    fn new(key: WakeKey) -> Self {
        Self {
            key,
            generation: 0,
            file_members: 0,
            parked: false,
            journal: None,
        }
    }

    fn unused(&self) -> bool {
        self.file_members == 0 && self.journal.is_none()
    }

    /// The key to wake once the table lock is released, if a waiter is parked.
    fn wake(&mut self) -> Option<WakeKey> {
        core::mem::take(&mut self.parked).then_some(self.key)
    }

    /// The node's generation moved; `advanced` says whether its journal did.
    /// Only a member that observes what moved is woken.
    fn changed(&mut self, advanced: bool) -> Option<WakeKey> {
        self.generation = self.generation.wrapping_add(1);
        if advanced || self.file_members > 0 {
            self.wake()
        } else {
            None
        }
    }
}

/// One volume's watched nodes.
pub struct VolumeWatch {
    volume: [u8; 16],
    matching: NameMatching,
    pressure: &'static MemoryPressure,
    registry: &'static WatchRegistry,
    /// How many nodes are watched. Every hook loads it first, so a volume
    /// nobody watches pays one load per mutation.
    watched: AtomicUsize,
    /// The claim of the wrapper feeding the table, `0` while none does.
    claimed: AtomicU64,
    /// The registry replaced the table: what its watchers watched is gone.
    retired: AtomicBool,
    nodes: SpinLock<HashMap<u64, NodeWatch, BuildSipHash13>>,
}

impl VolumeWatch {
    /// The watch table of volume `volume`, whose driver matches names as
    /// `matching`, belonging to `registry` but unpublished: a volume is
    /// watched through the table [`WatchRegistry::claim`] publishes.
    #[must_use]
    pub fn new(
        volume: [u8; 16],
        matching: NameMatching,
        pressure: &'static MemoryPressure,
        registry: &'static WatchRegistry,
    ) -> Self {
        Self {
            volume,
            matching,
            pressure,
            registry,
            watched: AtomicUsize::new(0),
            claimed: AtomicU64::new(0),
            retired: AtomicBool::new(false),
            nodes: SpinLock::new(HashMap::with_hasher(hasher())),
        }
    }

    /// The volume this table watches.
    #[must_use]
    pub const fn volume(&self) -> [u8; 16] {
        self.volume
    }

    fn idle(&self) -> bool {
        self.watched.load(Ordering::Acquire) == 0
    }

    fn precise(&self) -> bool {
        self.matching == NameMatching::Exact
    }

    /// The heap every journal together may hold, and one journal's equal share
    /// of it, so a watcher that never drains crowds out no other's journal.
    /// Pressure shrinks the budget as it does every record of filesystem
    /// metadata: the rescan a lost name costs is the dearer read.
    fn journal_limits(&self) -> (usize, usize) {
        let ceiling = CacheBudget::from_ceiling(self.pressure.total_bytes() / JOURNAL_SHARE);
        let budget = shrink_target(self.pressure.band(), ReclaimClass::FsMetadata, ceiling);
        let journals = self.registry.journal_count.load(Ordering::Relaxed).max(1);
        (budget, (budget / journals).min(JOURNAL_BYTES))
    }

    /// Hold every journal of the table to its share of the budget.
    fn shed_journals(&self) {
        if self.idle() {
            return;
        }
        let (_, share) = self.journal_limits();
        let mut nodes = self.nodes.lock();
        for journal in nodes
            .values_mut()
            .filter_map(|watch| watch.journal.as_mut())
        {
            journal.shed_to(share);
        }
    }

    /// Whether the registry replaced this table.
    #[must_use]
    pub fn retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    /// The epochs a path on this volume resolves against now, given the
    /// mount table's `mounts`.
    #[must_use]
    pub fn epochs(&self, mounts: u64) -> Epochs {
        self.registry.epochs(mounts)
    }

    /// An entry was added to or removed from `dir`: `dir`'s generation and its
    /// journal both move, and so does `dir`'s own entry in the directory
    /// holding it, `within`, when that is known — its stamp and link count
    /// changed.
    pub fn entries_changed(&self, dir: NodeId, name: &[u8], within: Option<(NodeId, &[u8])>) {
        if self.idle() {
            return;
        }
        let (precise, limits) = (self.precise(), self.journal_limits());
        let wakes = {
            let mut nodes = self.nodes.lock();
            let listed = nodes.get_mut(&dir.raw()).and_then(|watch| {
                let advanced = watch
                    .journal
                    .as_mut()
                    .is_some_and(|journal| journal.note(name, precise, limits) == Noted::Advanced);
                watch.changed(advanced)
            });
            let holder = within.and_then(|(parent, dir_name)| {
                let watch = nodes.get_mut(&parent.raw())?;
                match watch.journal.as_mut()?.note(dir_name, precise, limits) {
                    Noted::Advanced => watch.wake(),
                    Noted::Covered => None,
                }
            });
            [listed, holder]
        };
        wake_all(wakes);
    }

    /// `node` changed in a way only its File members observe: its link count
    /// or its metadata, with no name to attribute the change to.
    pub fn node_changed(&self, node: NodeId) {
        if self.idle() {
            return;
        }
        let wake = {
            let mut nodes = self.nodes.lock();
            nodes
                .get_mut(&node.raw())
                .and_then(|watch| watch.changed(false))
        };
        wake_all([wake, None]);
    }

    /// Whether any node of this volume is watched: the cache wrapper keeps its
    /// lookup trail only while one is.
    #[must_use]
    pub fn active(&self) -> bool {
        !self.idle()
    }

    /// The entry `name` of `dir` changed in place — its contents or its
    /// metadata. `node` is what it names, when known: its own generation moves.
    pub fn entry_changed(&self, dir: NodeId, name: &[u8], node: Option<NodeId>) {
        if self.idle() {
            return;
        }
        let (precise, limits) = (self.precise(), self.journal_limits());
        let wakes = {
            let mut nodes = self.nodes.lock();
            let named = node.and_then(|node| {
                nodes
                    .get_mut(&node.raw())
                    .and_then(|watch| watch.changed(false))
            });
            let listed = nodes.get_mut(&dir.raw()).and_then(|watch| {
                let journal = watch.journal.as_mut()?;
                match journal.note(name, precise, limits) {
                    Noted::Advanced => watch.wake(),
                    Noted::Covered => None,
                }
            });
            [named, listed]
        };
        wake_all(wakes);
    }

    /// `node` was removed, replaced or moved: what reaches it changed. Its
    /// watchers re-validate their path; its File members see a change.
    pub fn node_relocated(&self, node: NodeId) {
        if self.idle() {
            return;
        }
        let wake = {
            let mut nodes = self.nodes.lock();
            nodes.get_mut(&node.raw()).and_then(|watch| {
                let advanced = watch.journal.as_mut().is_some_and(|journal| {
                    journal.revalidate();
                    true
                });
                watch.changed(advanced)
            })
        };
        wake_all([wake, None]);
    }

    /// A directory or symbolic link was renamed, removed or replaced: a path
    /// through it may now reach somewhere else, or nowhere. Every watcher
    /// re-checks its path at its next drain.
    pub fn paths_moved(&self) {
        self.registry.moved(&self.registry.paths);
    }

    /// A directory's security changed: a watcher may have lost, or regained,
    /// the right to list what its journal names. Every watcher rescans, so no
    /// name recorded while it could not list the directory is ever delivered.
    pub fn access_moved(&self) {
        self.registry.moved(&self.registry.access);
    }

    /// Every watcher of this volume re-validates and rescans, and every File
    /// member sees a change: the volume left, or something under the whole
    /// volume changed that no name expresses. The caller wakes them.
    fn revalidate_all(&self) {
        if self.idle() {
            return;
        }
        let mut nodes = self.nodes.lock();
        for watch in nodes.values_mut() {
            watch.generation = watch.generation.wrapping_add(1);
            if let Some(journal) = watch.journal.as_mut() {
                journal.revalidate();
            }
            watch.parked = false;
        }
    }

    /// `node`'s entry, made first if nothing watched it yet.
    fn watched_node<'a>(
        &self,
        nodes: &'a mut HashMap<u64, NodeWatch, BuildSipHash13>,
        node: u64,
    ) -> Result<&'a mut NodeWatch, Errno> {
        if nodes.get(&node).is_none() {
            nodes
                .try_insert(node, NodeWatch::new(self.registry.mint_key()))
                .map_err(|_| Errno::OutOfMemory)?;
            self.watched.fetch_add(1, Ordering::Release);
        }
        nodes.get_mut(&node).ok_or(Errno::OutOfMemory)
    }

    /// Arm a watch on directory `node` for a path that resolved under
    /// `epochs`, returning the watcher's identity and whether the directory's
    /// journal is new.
    pub(crate) fn watch(&self, node: u64, epochs: Epochs) -> Result<(u64, bool), Errno> {
        let watcher = self.registry.next_watcher.fetch_add(1, Ordering::Relaxed);
        let mut nodes = self.nodes.lock();
        let watch = self.watched_node(&mut nodes, node)?;
        let created = watch.journal.is_none();
        let journal = watch
            .journal
            .get_or_insert_with(|| Journal::new(self.registry));
        if journal.join(watcher, epochs).is_ok() {
            return Ok((watcher, created));
        }
        if created {
            watch.journal = None;
        }
        self.drop_if_unused(&mut nodes, node);
        Err(Errno::OutOfMemory)
    }

    pub(crate) fn unwatch(&self, node: u64, watcher: u64) {
        let mut nodes = self.nodes.lock();
        if let Some(watch) = nodes.get_mut(&node) {
            if watch
                .journal
                .as_mut()
                .is_some_and(|journal| journal.leave(watcher))
            {
                watch.journal = None;
            }
        }
        self.drop_if_unused(&mut nodes, node);
    }

    fn drop_if_unused(&self, nodes: &mut HashMap<u64, NodeWatch, BuildSipHash13>, node: u64) {
        if nodes.get(&node).is_some_and(NodeWatch::unused) {
            nodes.remove(&node);
            self.watched.fetch_sub(1, Ordering::Release);
        }
    }

    /// Take what `watcher` has not drained from `node`'s journal, its records
    /// sized to fit `budget` bytes, or a rescan when `rescan`. `Ok(None)` for
    /// a watcher no longer armed.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] if the names cannot be copied out; nothing is
    /// taken.
    pub fn take(
        &self,
        node: u64,
        watcher: u64,
        budget: usize,
        rescan: bool,
    ) -> Result<Option<Drain>, Errno> {
        let mut nodes = self.nodes.lock();
        let Some(journal) = nodes.get_mut(&node).and_then(|w| w.journal.as_mut()) else {
            return Ok(None);
        };
        journal.take(watcher, budget, rescan)
    }

    /// Record that `watcher` was delivered everything up to `upto`, its path
    /// having resolved under `epochs`.
    pub fn commit(&self, node: u64, watcher: u64, upto: u64, epochs: Epochs) {
        let mut nodes = self.nodes.lock();
        if let Some(journal) = nodes.get_mut(&node).and_then(|w| w.journal.as_mut()) {
            journal.commit(watcher, upto, epochs);
        }
    }

    /// Add a File member watching `node`, returning its baseline generation.
    fn file_member_add(&self, node: u64) -> Result<u64, Errno> {
        let mut nodes = self.nodes.lock();
        let watch = self.watched_node(&mut nodes, node)?;
        watch.file_members += 1;
        Ok(watch.generation)
    }

    /// Drop one File member watching `node`.
    pub fn file_member_remove(&self, node: u64) {
        let mut nodes = self.nodes.lock();
        if let Some(watch) = nodes.get_mut(&node) {
            watch.file_members = watch.file_members.saturating_sub(1);
        }
        self.drop_if_unused(&mut nodes, node);
    }

    /// The current generation of `node`, `0` when it is not watched.
    #[must_use]
    pub fn generation(&self, node: u64) -> u64 {
        self.nodes.lock().get(&node).map_or(0, |w| w.generation)
    }

    /// The key a waiter on `node` parks under.
    #[must_use]
    pub fn key(&self, node: u64) -> Option<WakeKey> {
        self.nodes.lock().get(&node).map(|w| w.key)
    }

    /// A File member's readiness: its node changed since `observed`. Marks
    /// the node parked when it has not, so the next change wakes its key.
    #[must_use]
    pub fn file_ready(&self, node: u64, observed: u64) -> bool {
        let mut nodes = self.nodes.lock();
        let Some(watch) = nodes.get_mut(&node) else {
            return false;
        };
        if watch.generation != observed {
            return true;
        }
        watch.parked = true;
        false
    }

    /// A `DirWatch` member's state at `now`, given the mount table's epoch
    /// `mounts`. A member is due once its journal advanced — a retired table
    /// revalidates every journal — or an epoch its path resolves against moved.
    /// An epoch move is reported once and then held until the watcher drains,
    /// because until its drain re-resolves the path the directory may no
    /// longer be the watcher's to follow. Marks the node parked when the member
    /// is idle. A member whose watcher is no longer armed is idle for ever, as
    /// a closed descriptor is.
    #[must_use]
    pub fn dir_state(&self, node: u64, pacing: &Pacing, mounts: u64, now: u64) -> MemberState {
        let mut nodes = self.nodes.lock();
        let Some(watch) = nodes.get_mut(&node) else {
            return MemberState::Idle;
        };
        let Some(journal) = watch.journal.as_ref() else {
            return MemberState::Idle;
        };
        let Some(cursor) = journal.cursor(pacing.watcher) else {
            return MemberState::Idle;
        };
        // Decided under the epoch lock, so a move that lands after the epochs
        // are read finds the park and wakes it.
        let mut parked_for_epochs = self.registry.epoch_park.lock();
        let epochs = self.registry.epochs(mounts);
        if epochs != cursor.epochs {
            if pacing.epochs != cursor.epochs {
                return MemberState::Idle;
            }
        } else if journal.seq == pacing.observed {
            watch.parked = true;
            *parked_for_epochs = true;
            return MemberState::Idle;
        }
        drop(parked_for_epochs);
        let due = pacing
            .reported_at
            .map_or(0, |at| at.saturating_add(pacing.latency_ns));
        if now >= due {
            MemberState::Ready
        } else {
            MemberState::Due(due)
        }
    }

    /// The journal position a reported `DirWatch` member advances to.
    #[must_use]
    pub fn journal_seq(&self, node: u64) -> u64 {
        self.nodes
            .lock()
            .get(&node)
            .and_then(|w| w.journal.as_ref())
            .map_or(0, |j| j.seq)
    }

    fn watcher_cursor(&self, node: u64, watcher: u64) -> Option<Position> {
        self.nodes
            .lock()
            .get(&node)
            .and_then(|w| w.journal.as_ref())
            .and_then(|j| j.cursor(watcher))
    }

    /// Let go of claim `id`, if it is still the one feeding the table: the
    /// volume left, so every watcher re-validates and every File member sees
    /// a change.
    fn release(&self, id: u64) {
        if self
            .claimed
            .compare_exchange(id, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.revalidate_all();
            crate::waitq::fswatch_wake_all();
        }
    }

    /// The registry replaced this table: its watchers learn their directories
    /// are gone.
    fn retire(&self) {
        self.retired.store(true, Ordering::Release);
        self.revalidate_all();
        crate::waitq::fswatch_wake_all();
    }
}

fn hasher() -> BuildSipHash13 {
    // Names are chosen by whoever creates files, so the table hashes under the
    // per-boot key; a boot that never published one falls back as the futex
    // table does.
    BuildSipHash13::keyed().unwrap_or(BuildSipHash13::UNKEYED)
}

fn wake_all(keys: [Option<WakeKey>; 2]) {
    for key in keys.into_iter().flatten() {
        crate::waitq::fswatch_wake(key);
    }
}

/// The published tables, by volume id.
type VolumeTables = HashMap<[u8; 16], Arc<VolumeWatch>, BuildSipHash13>;

/// The key every `DirWatch` waiter also parks under, woken when an epoch
/// moves; node keys are minted after it.
const EPOCH_KEY: WakeKey = WakeKey::new(1);

/// Every volume's watch table, the watches each process holds, the heap every
/// journal holds together and the epochs every watched path resolves against:
/// one per kernel, handed to the syscall layer and to the volume bring-up that
/// claims tables.
pub struct WatchRegistry {
    volumes: SpinLock<Option<VolumeTables>>,
    /// Armed watches per process, for the `DirWatches` limit.
    charges: SpinLock<Option<HashMap<ProcessId, u64, BuildSipHash13>>>,
    /// The heap every journal holds, measured from the maps themselves.
    journal_bytes: AtomicUsize,
    /// How many journals share that heap.
    journal_count: AtomicUsize,
    /// [`Epochs::paths`].
    paths: AtomicU64,
    /// [`Epochs::access`].
    access: AtomicU64,
    /// A `DirWatch` member is parked until an epoch moves. The lock orders
    /// that decision against the move, so a move never passes a member
    /// parking on the epochs it replaced.
    epoch_park: SpinLock<bool>,
    next_key: AtomicU64,
    next_watcher: AtomicU64,
    next_claim: AtomicU64,
}

impl Default for WatchRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchRegistry {
    /// A registry holding no table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            volumes: SpinLock::new(None),
            charges: SpinLock::new(None),
            journal_bytes: AtomicUsize::new(0),
            journal_count: AtomicUsize::new(0),
            paths: AtomicU64::new(0),
            access: AtomicU64::new(0),
            epoch_park: SpinLock::new(false),
            next_key: AtomicU64::new(2),
            next_watcher: AtomicU64::new(1),
            next_claim: AtomicU64::new(1),
        }
    }

    fn mint_key(&self) -> WakeKey {
        WakeKey::new(self.next_key.fetch_add(1, Ordering::Relaxed))
    }

    /// The key a `DirWatch` waiter parks under for an epoch move.
    #[must_use]
    pub const fn epoch_key(&self) -> WakeKey {
        EPOCH_KEY
    }

    /// The epochs a watched path resolves against now, given the mount
    /// table's `mounts`. Read before the path resolves, so a move after it is
    /// the next drain's to see.
    #[must_use]
    pub fn epochs(&self, mounts: u64) -> Epochs {
        Epochs {
            mounts,
            paths: self.paths.load(Ordering::Acquire),
            access: self.access.load(Ordering::Acquire),
        }
    }

    /// `epoch` moved: whichever member parked on the epochs it replaced is
    /// woken, once however many moves follow before it parks again.
    fn moved(&self, epoch: &AtomicU64) {
        epoch.fetch_add(1, Ordering::AcqRel);
        if core::mem::take(&mut *self.epoch_park.lock()) {
            crate::waitq::fswatch_wake(EPOCH_KEY);
        }
    }

    /// Hold every journal to its share of the budget, now that one more
    /// journal shares it.
    fn share_out(&self) {
        let volumes = self.volumes.lock();
        for table in volumes.iter().flat_map(|map| map.values()) {
            table.shed_journals();
        }
    }

    /// Claim volume `volume`'s table for the cache wrapper about to feed it,
    /// publishing one if the volume has none: a volume that returns is watched
    /// through the table it left, so its watchers carry on.
    ///
    /// [`None`] while a live wrapper already feeds a volume of the same
    /// identity — a second device carrying the same volume id must not report
    /// its changes to the first one's watchers — or if the registry cannot
    /// grow. A table left behind by a volume whose names matched differently
    /// is retired.
    #[must_use]
    pub fn claim(
        &'static self,
        volume: [u8; 16],
        matching: NameMatching,
        pressure: &'static MemoryPressure,
    ) -> Option<Claim> {
        let id = self.next_claim.fetch_add(1, Ordering::Relaxed);
        let (table, retired) = {
            let mut volumes = self.volumes.lock();
            let map = volumes.get_or_insert_with(|| HashMap::with_hasher(hasher()));
            // A table nothing feeds and nothing else holds watches nothing.
            map.retain(|_, table| {
                Arc::strong_count(table) > 1 || table.claimed.load(Ordering::Acquire) != 0
            });
            if map
                .get(&volume)
                .is_some_and(|table| table.claimed.load(Ordering::Acquire) != 0)
            {
                return None;
            }
            let reused = map
                .get(&volume)
                .filter(|table| table.matching == matching)
                .cloned();
            let (table, retired) = if let Some(table) = reused {
                (Some(table), None)
            } else {
                let table = Arc::new(VolumeWatch::new(volume, matching, pressure, self));
                // A table left behind is replaced in place, which never
                // allocates, so it is always handed back to be retired; only
                // a volume with none can be refused.
                match map.try_insert(volume, Arc::clone(&table)) {
                    Ok(replaced) => (Some(table), replaced),
                    Err(_) => (None, None),
                }
            };
            if let Some(table) = &table {
                table.claimed.store(id, Ordering::Release);
            }
            (table, retired)
        };
        if let Some(old) = retired {
            old.retire();
        }
        table.map(|table| Claim { table, id })
    }

    /// The table of volume `volume`, if one is published.
    #[must_use]
    pub fn volume(&self, volume: [u8; 16]) -> Option<Arc<VolumeWatch>> {
        self.volumes.lock().as_ref()?.get(&volume).cloned()
    }

    /// How many watches `process` holds armed.
    #[must_use]
    pub fn usage(&self, process: ProcessId) -> u64 {
        self.charges
            .lock()
            .as_ref()
            .and_then(|map| map.get(&process).copied())
            .unwrap_or(0)
    }

    fn charge(&self, process: ProcessId, limit: u64) -> Result<(), Errno> {
        let mut charges = self.charges.lock();
        let map = charges.get_or_insert_with(|| HashMap::with_hasher(hasher()));
        let held = map.get(&process).copied().unwrap_or(0);
        if held >= limit {
            return Err(Errno::LimitExceeded);
        }
        if let Some(count) = map.get_mut(&process) {
            *count += 1;
            return Ok(());
        }
        map.try_insert(process, 1).map_err(|_| Errno::OutOfMemory)?;
        Ok(())
    }

    fn uncharge(&self, process: ProcessId) {
        let mut charges = self.charges.lock();
        let Some(map) = charges.as_mut() else {
            return;
        };
        let emptied = map.get_mut(&process).is_some_and(|count| {
            *count = count.saturating_sub(1);
            *count == 0
        });
        if emptied {
            map.remove(&process);
        }
    }

    /// Add a File member on node `file`: its volume's table, which the member
    /// holds for its life, and the node's baseline generation.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for a node on a volume with no published table —
    /// no change to it could ever be reported — or [`Errno::OutOfMemory`].
    pub fn file_member_add(&self, file: FileId) -> Result<(Arc<VolumeWatch>, u64), Errno> {
        let table = self.volume(file.volume).ok_or(Errno::NotFound)?;
        let baseline = table.file_member_add(file.node)?;
        Ok((table, baseline))
    }
}

/// A volume's table, held by the one cache wrapper feeding it. Dropping it
/// lets the table go, as [`ClaimRef::release`] does.
pub struct Claim {
    table: Arc<VolumeWatch>,
    id: u64,
}

impl Claim {
    /// The claimed table.
    #[must_use]
    pub fn table(&self) -> &Arc<VolumeWatch> {
        &self.table
    }

    /// A handle that releases this claim without owning it — what the
    /// registration of the volume keeps, so the claim ends when the volume is
    /// unregistered rather than when its last operation finishes.
    #[must_use]
    pub fn reference(&self) -> ClaimRef {
        ClaimRef {
            table: Arc::clone(&self.table),
            id: self.id,
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.table.release(self.id);
    }
}

/// Releases one [`Claim`] on behalf of its volume's registration.
#[derive(Clone)]
pub struct ClaimRef {
    table: Arc<VolumeWatch>,
    id: u64,
}

impl ClaimRef {
    /// End the claim, if it is still the table's. Idempotent.
    pub fn release(&self) {
        self.table.release(self.id);
    }
}

/// A mount table changed: a mount point may have appeared in a listing, or a
/// watched path may now reach another directory. Every member re-checks the
/// table's epoch, and a drain rescans only a directory whose own mounts
/// moved.
///
/// Called with the mount table's lock held, so the wake is the deferred
/// broadcast rather than a direct unpark.
pub fn mounts_changed() {
    crate::waitq::FSWATCH_WAITQ.request_wake();
}

/// A watch armed on an open directory description, released with it.
pub struct ArmedWatch {
    registry: &'static WatchRegistry,
    table: Arc<VolumeWatch>,
    node: u64,
    watcher: u64,
    latency_ns: u64,
    owner: ProcessId,
    /// The mounts directly beneath the directory when its path last
    /// resolved; the epochs it resolved under are its cursor's.
    children: SpinLock<ChildMounts>,
}

impl core::fmt::Debug for ArmedWatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ArmedWatch")
            .field("node", &self.node)
            .field("watcher", &self.watcher)
            .field("latency_ns", &self.latency_ns)
            .finish_non_exhaustive()
    }
}

impl ArmedWatch {
    /// Arm a watch on directory `dir` for `owner`, charged against its
    /// `limit`. `resolved` is what its path resolved under: epochs read before
    /// the path resolved, so a move during the resolution reports at once,
    /// and the mounts its listing showed beneath it.
    ///
    /// # Errors
    ///
    /// [`Errno::NotImplemented`] for a volume with no published table (a
    /// directory no mutation could ever be recorded against),
    /// [`Errno::LimitExceeded`] past `limit`, [`Errno::OutOfMemory`].
    pub fn arm(
        registry: &'static WatchRegistry,
        dir: FileId,
        latency_ns: u64,
        owner: ProcessId,
        limit: u64,
        resolved: Resolved,
    ) -> Result<Self, Errno> {
        let table = registry.volume(dir.volume).ok_or(Errno::NotImplemented)?;
        registry.charge(owner, limit)?;
        match table.watch(dir.node, resolved.epochs) {
            Ok((watcher, created)) => {
                if created {
                    registry.share_out();
                }
                Ok(Self {
                    registry,
                    table,
                    node: dir.node,
                    watcher,
                    latency_ns,
                    owner,
                    children: SpinLock::new(resolved.children),
                })
            }
            Err(err) => {
                registry.uncharge(owner);
                Err(err)
            }
        }
    }

    /// The watched directory.
    #[must_use]
    pub fn dir(&self) -> FileId {
        FileId {
            volume: self.table.volume,
            node: self.node,
        }
    }

    /// The table of the volume the watched directory is on.
    #[must_use]
    pub fn table(&self) -> &Arc<VolumeWatch> {
        &self.table
    }

    /// The watcher's identity, unique for the life of the kernel.
    #[must_use]
    pub const fn watcher(&self) -> u64 {
        self.watcher
    }

    /// The least interval between two reports of a member on this watch.
    #[must_use]
    pub const fn latency_ns(&self) -> u64 {
        self.latency_ns
    }

    /// Take what has not been drained, sized to `budget`, or a rescan when
    /// `rescan`.
    ///
    /// # Errors
    ///
    /// As [`VolumeWatch::take`].
    pub fn take(&self, budget: usize, rescan: bool) -> Result<Option<Drain>, Errno> {
        self.table.take(self.node, self.watcher, budget, rescan)
    }

    /// Advance the cursor past what was delivered, the path having resolved
    /// under `resolved`.
    pub fn commit(&self, upto: u64, resolved: Resolved) {
        self.table
            .commit(self.node, self.watcher, upto, resolved.epochs);
        *self.children.lock() = resolved.children;
    }

    /// The mounts directly beneath the directory when its path last resolved.
    #[must_use]
    pub fn children(&self) -> ChildMounts {
        *self.children.lock()
    }

    /// Retire the watch once its directory is gone or no longer the
    /// watcher's to list: it records nothing more and its members never
    /// report again, though it stays charged until its descriptor closes.
    pub fn spend(&self) {
        self.table.unwatch(self.node, self.watcher);
    }

    /// Where this watcher's cursor stands — the baseline a new member takes,
    /// so changes recorded before it was added still report — or [`None`]
    /// once the watch is spent.
    #[must_use]
    pub fn position(&self) -> Option<Position> {
        self.table.watcher_cursor(self.node, self.watcher)
    }
}

impl Drop for ArmedWatch {
    fn drop(&mut self) {
        self.table.unwatch(self.node, self.watcher);
        self.registry.uncharge(self.owner);
    }
}

#[cfg(test)]
#[path = "fswatch_tests.rs"]
mod tests;
