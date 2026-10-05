//! Live listings: what a directory watch reports, in the engine's terms
//! (`docs/src/filesystem/watch.md`).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::Errno;
#[cfg(any(feature = "rt", test))]
use tairix_abi::{DirChange, DirChangeBatch, DirWatchStatus};

use crate::entry::Entry;
use crate::sort::{entry_cmp, SortMode};
#[cfg(any(feature = "rt", test))]
use crate::vfs::{entry_from_record, LinkReader};

/// The latency every desktop listing arms its watch with: a lone change
/// shows at once, and a storm repaints at most five times a second.
pub const WATCH_LATENCY_NS: u64 = 200_000_000;

/// The scratch a reader drains watches through, held for its life: a few
/// hundred changed names a batch, with more arriving as further batches.
pub const WATCH_BUFFER_LEN: usize = 64 * 1024;

/// How many changed entries a consumer holds before collapsing them to a
/// rescan: past this, re-reading the directory is the cheaper answer.
pub(crate) const PENDING_CHANGES_MAX: usize = 4096;

/// One entry a watch reported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntryChange {
    /// The entry exists, as it is now.
    Upsert(Entry),
    /// No entry of this name exists.
    Remove(String),
}

/// What draining a watch amounts to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum WatchUpdate {
    /// Nothing changed.
    #[default]
    Quiet,
    /// These entries changed, in the order reported.
    Changes(Vec<EntryChange>),
    /// Re-read the whole directory.
    Rescan,
    /// The path no longer reaches the watched directory.
    Gone,
}

impl WatchUpdate {
    /// Fold a later update into this one. Gone outranks everything, a rescan
    /// outranks changes, and changes accumulate until
    /// `PENDING_CHANGES_MAX`, beyond which they collapse to a rescan.
    pub fn absorb(&mut self, later: Self) {
        let merged = match (core::mem::take(self), later) {
            (Self::Gone, _) | (_, Self::Gone) => Self::Gone,
            (Self::Rescan, _) | (_, Self::Rescan) => Self::Rescan,
            (Self::Quiet, other) | (other, Self::Quiet) => other,
            (Self::Changes(mut held), Self::Changes(more)) => {
                if held.len() + more.len() > PENDING_CHANGES_MAX
                    || held.try_reserve(more.len()).is_err()
                {
                    Self::Rescan
                } else {
                    held.extend(more);
                    Self::Changes(held)
                }
            }
        };
        *self = merged;
    }
}

/// Decode one `fs_watch_read` answer for the directory spelled `directory`,
/// answering the update it carries and whether more batches wait.
///
/// A present record becomes the [`Entry`] a listing of the directory would
/// hold, through the same per-record decode, links resolved by `links`.
///
/// # Errors
///
/// Whatever [`DirChangeBatch::decode`] refuses, or [`Errno::OutOfRange`] for a
/// name that is not UTF-8. A refused batch is refused whole.
#[cfg(any(feature = "rt", test))]
pub(crate) fn decode_batch(
    directory: &str,
    bytes: &[u8],
    links: &mut dyn LinkReader,
) -> Result<(WatchUpdate, bool), Errno> {
    let batch = DirChangeBatch::decode(bytes)?;
    let update = match batch.status {
        DirWatchStatus::Gone => WatchUpdate::Gone,
        DirWatchStatus::Rescan => WatchUpdate::Rescan,
        DirWatchStatus::Changes => {
            let mut changes = Vec::new();
            for change in batch.changes() {
                changes.push(match change {
                    DirChange::Present(record) => {
                        EntryChange::Upsert(entry_from_record(directory, &record, links)?)
                    }
                    DirChange::Absent(name) => EntryChange::Remove(String::from(
                        core::str::from_utf8(name).map_err(|_| Errno::OutOfRange)?,
                    )),
                });
            }
            if changes.is_empty() {
                WatchUpdate::Quiet
            } else {
                WatchUpdate::Changes(changes)
            }
        }
    };
    Ok((update, batch.more))
}

/// Where each entry a listing held before a change now sits, for a caller to
/// carry what it tracks by position — a selection, a hover, a half-made
/// double-click — onto the same entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Placement {
    before: usize,
    placed: Placed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Placed {
    /// Each earlier index's new one.
    Table(Vec<Option<usize>>),
    /// Every entry not in `gone` kept its order among the others, so what a
    /// merge moved is all it costs.
    Merged {
        /// Each entry taken out, by its earlier index in ascending order, with
        /// where it was put back, if it was.
        gone: Vec<(usize, Option<usize>)>,
        /// For each entry put in, in listing order, how many kept entries
        /// precede it.
        arrived: Vec<usize>,
    },
}

impl Placement {
    /// Where the entry that sat at `before` sits now: [`None`] once it is
    /// gone, or for an index the listing never held.
    #[must_use]
    pub fn place(&self, before: usize) -> Option<usize> {
        if before >= self.before {
            return None;
        }
        match &self.placed {
            Placed::Table(table) => table.get(before).copied().flatten(),
            Placed::Merged { gone, arrived } => {
                match gone.binary_search_by_key(&before, |&(at, _)| at) {
                    Ok(found) => gone.get(found).and_then(|&(_, to)| to),
                    Err(taken_ahead) => {
                        let rank = before - taken_ahead;
                        Some(rank + arrived.partition_point(|&kept| kept <= rank))
                    }
                }
            }
        }
    }

    /// [`place`](Self::place) for a position carried as a click subject, as
    /// a listing's surfaces pair presses on an item's index.
    #[must_use]
    pub fn place_subject(&self, subject: u64) -> Option<u64> {
        let at = usize::try_from(subject).ok()?;
        u64::try_from(self.place(at)?).ok()
    }

    pub(crate) fn table(table: Vec<Option<usize>>) -> Self {
        Self {
            before: table.len(),
            placed: Placed::Table(table),
        }
    }
}

/// How many entries a merge puts into the listing where they belong before it
/// rebuilds the listing instead: an insert moves the tail once, a rebuild moves
/// every entry into a fresh listing-sized allocation, so a few inserts are the
/// cheaper of the two.
const INSERTS_IN_PLACE: usize = 4;

/// One name a batch of changes touched: the change with the last word on it,
/// and where the listing held that name before.
struct Touched<'a> {
    name: &'a str,
    change: usize,
    held: Option<usize>,
}

/// What one change does to the listing.
#[derive(Copy, Clone)]
enum Fate {
    /// Nothing shown changes.
    Untouched,
    /// The entry at this index goes.
    Removed(usize),
    /// A new entry comes in.
    Arrived,
    /// The entry at this index is replaced where it stands.
    Replaced(usize),
    /// The entry at this index is replaced somewhere else in the order.
    Moved(usize),
}

fn change_name(change: &EntryChange) -> &str {
    match change {
        EntryChange::Upsert(entry) => entry.name(),
        EntryChange::Remove(name) => name,
    }
}

fn fate(change: &EntryChange, held: Option<usize>, entries: &[Entry], mode: SortMode) -> Fate {
    let was = held.and_then(|at| Some((at, entries.get(at)?)));
    match (change, was) {
        (EntryChange::Remove(_), None) => Fate::Untouched,
        (EntryChange::Remove(_), Some((at, _))) => Fate::Removed(at),
        (EntryChange::Upsert(_), None) => Fate::Arrived,
        // A reported folder is probed again even when its record reads the
        // same: its own stamp may be too coarse to show the change.
        (EntryChange::Upsert(fresh), Some((_, was)))
            if fresh.same_listing(was) && !fresh.is_directory() =>
        {
            Fate::Untouched
        }
        (EntryChange::Upsert(fresh), Some((at, was))) if entry_cmp(was, fresh, mode).is_eq() => {
            Fate::Replaced(at)
        }
        (EntryChange::Upsert(_), Some((at, _))) => Fate::Moved(at),
    }
}

/// A filter's bits per name it holds: with two probes, about one name in
/// seventy that the filter was not given still goes on to the search.
const FILTER_BITS_PER_NAME: usize = 16;

/// A filter over a batch's names, so an entry the batch did not name — almost
/// every one — is passed over without searching the batch for it: first by
/// its length, which costs nothing, then by a Bloom filter.
///
/// Its hash is not keyed and needs no key: a name crafted to get past it costs
/// only the search it would have skipped.
struct NameFilter {
    /// A bit per name length the batch holds, the last standing for every
    /// length from it up.
    lengths: u64,
    words: Vec<u64>,
}

fn length_bit(name: &str) -> u64 {
    1 << name.len().min(u64::BITS as usize - 1)
}

impl NameFilter {
    /// A filter holding `names`, or [`None`] without the memory for one.
    fn of<'a>(names: impl ExactSizeIterator<Item = &'a str>) -> Option<Self> {
        let bits = names
            .len()
            .saturating_mul(FILTER_BITS_PER_NAME)
            .checked_next_power_of_two()?
            .max(u64::BITS as usize);
        let mut words = Vec::new();
        words.try_reserve_exact(bits / u64::BITS as usize).ok()?;
        words.resize(bits / u64::BITS as usize, 0);
        let mut filter = Self { lengths: 0, words };
        for name in names {
            filter.lengths |= length_bit(name);
            for (word, bit) in filter.probes(name) {
                if let Some(word) = filter.words.get_mut(word) {
                    *word |= bit;
                }
            }
        }
        Some(filter)
    }

    /// Whether `name` may be one the filter holds.
    fn may_hold(&self, name: &str) -> bool {
        self.lengths & length_bit(name) != 0
            && self
                .probes(name)
                .iter()
                .all(|&(word, bit)| self.words.get(word).is_some_and(|word| word & bit != 0))
    }

    /// The word and bit of each of `name`'s two probes.
    fn probes(&self, name: &str) -> [(usize, u64); 2] {
        let hash = tairix_hash::FastHash::hash_bytes(0, name.as_bytes());
        let bits = (self.words.len() * u64::BITS as usize) as u64;
        [hash % bits, (hash >> 32) % bits].map(|bit| {
            let word = usize::try_from(bit / u64::from(u64::BITS)).unwrap_or(usize::MAX);
            (word, 1 << (bit % u64::from(u64::BITS)))
        })
    }
}

/// What each of `changes` does to `entries`: the latest change to a name has
/// the last word, and an earlier one does nothing.
fn fates(entries: &[Entry], changes: &[EntryChange], mode: SortMode) -> Option<Vec<Fate>> {
    let mut touched = Vec::new();
    touched.try_reserve_exact(changes.len()).ok()?;
    touched.extend(changes.iter().enumerate().map(|(change, named)| Touched {
        name: change_name(named),
        change,
        held: None,
    }));
    touched.sort_unstable_by(|a, b| a.name.cmp(b.name).then(b.change.cmp(&a.change)));
    touched.dedup_by(|later, latest| later.name == latest.name);
    let filter = NameFilter::of(touched.iter().map(|touch| touch.name))?;
    let mut unfound = touched.len();
    for (at, entry) in entries.iter().enumerate() {
        if unfound == 0 {
            break;
        }
        if !filter.may_hold(entry.name()) {
            continue;
        }
        let found = touched.binary_search_by(|probe| probe.name.cmp(entry.name()));
        if let Some(touch) = found.ok().and_then(|found| touched.get_mut(found)) {
            touch.held = Some(at);
            unfound -= 1;
        }
    }
    let mut fates = Vec::new();
    fates.try_reserve_exact(changes.len()).ok()?;
    fates.resize(changes.len(), Fate::Untouched);
    for touch in &touched {
        if let (Some(slot), Some(change)) = (fates.get_mut(touch.change), changes.get(touch.change))
        {
            *slot = fate(change, touch.held, entries, mode);
        }
    }
    Some(fates)
}

/// Merge the `changes` a watch reported into `entries`, a listing in `mode`'s
/// order: the latest change to a name wins, an entry a change left as it was
/// keeps its occupancy untouched, and a changed folder keeps showing its
/// occupancy until probed again.
///
/// One pass over the listing finds every entry the changes name, and an entry
/// that keeps its place is replaced where it stands; what goes, arrives or
/// moves is merged in order, so the listing is moved at most once whatever the
/// batch holds. Everything the merge needs is allocated before the listing is
/// touched.
///
/// Answers where each entry that was there before now sits and whether
/// anything shown moved; or [`None`], leaving `entries` as they were, when the
/// memory to merge could not be had and only reading the directory again
/// brings it up to date.
pub fn merge_changes(
    entries: &mut Vec<Entry>,
    changes: Vec<EntryChange>,
    mode: SortMode,
) -> Option<(Placement, bool)> {
    let before = entries.len();
    let fates = fates(entries, &changes, mode)?;
    let (mut leaving, mut arriving) = (0, 0);
    for fate in &fates {
        match fate {
            Fate::Removed(_) => leaving += 1,
            Fate::Arrived => arriving += 1,
            Fate::Moved(_) => {
                leaving += 1;
                arriving += 1;
            }
            Fate::Untouched | Fate::Replaced(_) => {}
        }
    }
    let mut gone = Vec::new();
    gone.try_reserve_exact(leaving).ok()?;
    let mut incoming = Vec::new();
    incoming.try_reserve_exact(arriving).ok()?;
    let mut arrived = Vec::new();
    arrived.try_reserve_exact(arriving).ok()?;
    let in_place = arriving <= INSERTS_IN_PLACE;
    let mut merged = Vec::new();
    if in_place {
        entries.try_reserve(arriving).ok()?;
    } else {
        merged.try_reserve_exact(before - leaving + arriving).ok()?;
    }

    let mut moved = false;
    for (change, fate) in changes.into_iter().zip(fates) {
        match (fate, change) {
            (Fate::Removed(at), _) => {
                gone.push((at, None));
                moved = true;
            }
            (Fate::Arrived, EntryChange::Upsert(fresh)) => {
                incoming.push((None, fresh));
                moved = true;
            }
            (Fate::Replaced(at) | Fate::Moved(at), EntryChange::Upsert(mut fresh)) => {
                let Some(was) = entries.get_mut(at) else {
                    continue;
                };
                moved |= !fresh.same_listing(was);
                fresh.inherit_occupancy(was);
                if matches!(fate, Fate::Replaced(_)) {
                    *was = fresh;
                } else {
                    gone.push((at, None));
                    incoming.push((Some(at), fresh));
                }
            }
            _ => {}
        }
    }
    gone.sort_unstable_by_key(|&(at, _)| at);
    incoming.sort_unstable_by(|a: &(Option<usize>, Entry), b| entry_cmp(&a.1, &b.1, mode));
    if in_place {
        insert_in_place(entries, &mut gone, incoming, &mut arrived, mode);
    } else {
        let old = core::mem::take(entries);
        *entries = rebuild(old, &mut gone, incoming, &mut arrived, merged, mode);
    }
    let placement = Placement {
        before,
        placed: Placed::Merged { gone, arrived },
    };
    Some((placement, moved))
}

/// Put `fresh`, which sat at `from` if anywhere, at the end of `merged`.
fn arrive(
    merged: &mut Vec<Entry>,
    gone: &mut [(usize, Option<usize>)],
    arrived: &mut Vec<usize>,
    (from, fresh): (Option<usize>, Entry),
) {
    put_back(gone, from, merged.len());
    arrived.push(merged.len() - arrived.len());
    merged.push(fresh);
}

/// Note that the entry that sat at `from`, if any, was put back at `to`.
fn put_back(gone: &mut [(usize, Option<usize>)], from: Option<usize>, to: usize) {
    let found = from.and_then(|at| gone.binary_search_by_key(&at, |&(at, _)| at).ok());
    if let Some(slot) = found.and_then(|found| gone.get_mut(found)) {
        slot.1 = Some(to);
    }
}

/// Take `gone` out of `entries` and insert `incoming`, in order, where each
/// belongs; the room for them is already reserved.
fn insert_in_place(
    entries: &mut Vec<Entry>,
    gone: &mut [(usize, Option<usize>)],
    incoming: Vec<(Option<usize>, Entry)>,
    arrived: &mut Vec<usize>,
    mode: SortMode,
) {
    // A few go by moving the tail once each, more by one compacting pass.
    if gone.len() <= INSERTS_IN_PLACE {
        for &(at, _) in gone.iter().rev() {
            if at < entries.len() {
                drop(entries.remove(at));
            }
        }
    } else {
        let (mut at, mut next) = (0, 0);
        entries.retain(|_| {
            let taken = gone.get(next).is_some_and(|&(leaving, _)| leaving == at);
            next += usize::from(taken);
            at += 1;
            !taken
        });
    }
    // Each lands after the ones already put in, so every kept entry ahead of
    // it is one of `to`'s.
    for (ahead, (from, fresh)) in incoming.into_iter().enumerate() {
        let to = entries.partition_point(|entry| entry_cmp(entry, &fresh, mode).is_lt());
        entries.insert(to, fresh);
        arrived.push(to - ahead);
        put_back(gone, from, to);
    }
}

/// Merge what `old` keeps with `incoming` into `merged`, which has room for
/// all of it.
fn rebuild(
    old: Vec<Entry>,
    gone: &mut [(usize, Option<usize>)],
    incoming: Vec<(Option<usize>, Entry)>,
    arrived: &mut Vec<usize>,
    mut merged: Vec<Entry>,
    mode: SortMode,
) -> Vec<Entry> {
    let mut incoming = incoming.into_iter().peekable();
    let mut next = 0;
    for (at, entry) in old.into_iter().enumerate() {
        if gone.get(next).is_some_and(|&(leaving, _)| leaving == at) {
            next += 1;
            continue;
        }
        while let Some(fresh) =
            incoming.next_if(|(_, fresh)| entry_cmp(fresh, &entry, mode).is_lt())
        {
            arrive(&mut merged, gone, arrived, fresh);
        }
        merged.push(entry);
    }
    for fresh in incoming {
        arrive(&mut merged, gone, arrived, fresh);
    }
    merged
}

/// Each listing consumer's directory watch: the armed directory a listing
/// read handed over, the drains asked for, and what they produced.
///
/// Free of locks, threads, and syscalls, so every rule is a host test; the
/// embedder supplies the exclusion and the blocking. `C` names a consumer and
/// `H` is the armed directory, shared with a drain while one runs.
#[derive(Debug)]
pub struct Watches<C, H> {
    slots: BTreeMap<C, WatchSlot<H>>,
    stopping: bool,
}

#[derive(Debug)]
struct WatchSlot<H> {
    /// A directory armed and listed by the reader, until the window commits
    /// the listing it came with.
    offered: Option<(Vec<String>, H)>,
    /// What the window watches now.
    watching: Option<(Vec<String>, H)>,
    /// A drain the loop asked for and the reader has not taken.
    wanted: bool,
    /// What drains produced and the window has not adopted.
    landed: WatchUpdate,
}

impl<H> Default for WatchSlot<H> {
    fn default() -> Self {
        Self {
            offered: None,
            watching: None,
            wanted: false,
            landed: WatchUpdate::Quiet,
        }
    }
}

/// What a consumer taking a listing did to its watch.
#[derive(Debug, Eq, PartialEq)]
pub struct Took<H> {
    /// The watch armed with the listing, now the one the consumer reports on:
    /// join it to the consumer's wait-set.
    pub join: Option<H>,
    /// What the consumer no longer reports on — the watch it moved away from,
    /// a stale offer — to let go of outside the caller's lock.
    pub release: [Option<H>; 2],
}

#[cfg(feature = "rt")]
impl Took<alloc::sync::Arc<WatchedDirectory>> {
    /// Let go of what the consumer no longer reports on and join the watch it
    /// now does to `set` as `token`.
    ///
    /// # Errors
    ///
    /// As [`WatchedDirectory::join`]: the listing is correct but not live, and
    /// the consumer unwatches it.
    pub fn commit(self, set: u64, token: u64) -> Result<(), Errno> {
        drop(self.release);
        self.join.map_or(Ok(()), |dir| dir.join(set, token))
    }
}

impl<H> Default for Took<H> {
    fn default() -> Self {
        Self {
            join: None,
            release: [None, None],
        }
    }
}

impl<C: Ord, H> Default for Watches<C, H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Ord, H> Watches<C, H> {
    /// A desk watching nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: BTreeMap::new(),
            stopping: false,
        }
    }
}

impl<C: Copy + Ord, H: Clone> Watches<C, H> {
    /// The reader listed `target` for `client` through `handle`, armed: hold
    /// it for the consumer to install when it commits that listing. Offer only
    /// a listing the consumer still wants — one the listing desk accepted —
    /// so a forgotten consumer's watch is never held. Answers the handle this
    /// displaced, or `handle` itself when nothing is recorded any more, for
    /// the caller to let go of outside its lock.
    pub fn offer(&mut self, client: C, target: &[String], handle: H) -> Option<H> {
        if self.stopping {
            return Some(handle);
        }
        self.slots
            .entry(client)
            .or_default()
            .offered
            .replace((target.to_vec(), handle))
            .map(|(_, displaced)| displaced)
    }

    /// A listing of `target` read for `client` reached the listing desk, which
    /// still wanted it when `owed`: the watch `armed` with it is offered with
    /// it, and answered back — as one an offer displaced is — when it was not,
    /// for the caller to let go of outside its lock.
    pub fn offer_armed(
        &mut self,
        owed: bool,
        client: C,
        target: &[String],
        armed: Option<H>,
    ) -> Option<H> {
        match armed {
            Some(handle) if owed => self.offer(client, target, handle),
            unwanted => unwanted,
        }
    }

    /// The reader is about to list `location` for `client`. When the consumer
    /// already holds a watch there — watched, or offered with a listing not
    /// yet committed — the read goes through it rather than arming another,
    /// so a reload keeps the watch's pacing; and what its drains had landed is
    /// superseded by the read and dropped. A watch that reported its directory
    /// gone is spent, so it is never read through again: the read arms afresh,
    /// and the gone stays landed for a read that finds nothing either.
    pub fn relisting(&mut self, client: C, location: &[String]) -> Option<H> {
        let slot = self.slots.get_mut(&client)?;
        let watched_here = slot
            .watching
            .as_ref()
            .is_some_and(|(at, _)| at.as_slice() == location);
        let spent = watched_here && slot.landed == WatchUpdate::Gone;
        let live = [&slot.offered, if spent { &None } else { &slot.watching }];
        let handle = live
            .into_iter()
            .flatten()
            .find(|(at, _)| at.as_slice() == location)
            .map(|(_, handle)| handle.clone())?;
        if watched_here && !spent {
            slot.landed = WatchUpdate::Quiet;
        }
        Some(handle)
    }

    /// `client` took the listing of `location` it asked for. The watch armed
    /// with it is installed; an offer for anywhere else is stale and goes;
    /// and a watch of somewhere the consumer no longer shows is let go, with
    /// whatever its drains had landed.
    pub fn took(&mut self, client: C, location: &[String]) -> Took<H> {
        let Some(slot) = self.slots.get_mut(&client) else {
            return Took::default();
        };
        match slot.offered.take() {
            Some((target, handle)) if target == location => {
                let replaced = slot
                    .watching
                    .replace((target, handle.clone()))
                    .map(|(_, old)| old);
                slot.landed = WatchUpdate::Quiet;
                slot.wanted = false;
                Took {
                    join: Some(handle),
                    release: [replaced, None],
                }
            }
            stale => {
                let left = if slot
                    .watching
                    .as_ref()
                    .is_some_and(|(at, _)| at.as_slice() != location)
                {
                    slot.landed = WatchUpdate::Quiet;
                    slot.wanted = false;
                    slot.watching.take().map(|(_, handle)| handle)
                } else {
                    None
                };
                Took {
                    join: None,
                    release: [stale.map(|(_, handle)| handle), left],
                }
            }
        }
    }

    /// Whether `client`'s watch follows `location`, so a change made there
    /// is reported to it and a consumer that has just made one need not read
    /// the directory again. A watch on another folder, or one that reported
    /// its directory gone, follows nothing here.
    #[must_use]
    pub fn follows(&self, client: C, location: &[String]) -> bool {
        self.slots.get(&client).is_some_and(|slot| {
            slot.landed != WatchUpdate::Gone
                && slot
                    .watching
                    .as_ref()
                    .is_some_and(|(at, _)| at.as_slice() == location)
        })
    }

    /// The window's watch reported a change: ask the reader to drain it.
    /// Answers whether this recorded work, so the reader is woken once.
    pub fn want_drain(&mut self, client: C) -> bool {
        match self.slots.get_mut(&client) {
            Some(slot) if !self.stopping && slot.watching.is_some() && !slot.wanted => {
                slot.wanted = true;
                true
            }
            _ => false,
        }
    }

    /// The next drain for the reader: whose, which location, and the handle.
    pub fn next_drain(&mut self) -> Option<(C, Vec<String>, H)> {
        if self.stopping {
            return None;
        }
        self.slots.iter_mut().find_map(|(client, slot)| {
            if !slot.wanted {
                return None;
            }
            slot.wanted = false;
            let (location, handle) = slot.watching.as_ref()?;
            Some((*client, location.clone(), handle.clone()))
        })
    }

    /// A drain of `location` for `client` produced `update`. Kept only while
    /// the window still watches there. Answers whether the loop is owed a
    /// wake.
    pub fn deliver(&mut self, client: C, location: &[String], update: WatchUpdate) -> bool {
        let Some(slot) = self.slots.get_mut(&client) else {
            return false;
        };
        if update == WatchUpdate::Quiet
            || slot.watching.as_ref().map(|(at, _)| at.as_slice()) != Some(location)
        {
            return false;
        }
        slot.landed.absorb(update);
        true
    }

    /// What the window's drains produced, handed over once.
    pub fn take_update(&mut self, client: C) -> WatchUpdate {
        self.slots
            .get_mut(&client)
            .map(|slot| core::mem::take(&mut slot.landed))
            .unwrap_or_default()
    }

    /// The window stopped watching — its directory went, or it moved
    /// elsewhere: the watch it held, for the window to release.
    pub fn unwatch(&mut self, client: C) -> Option<H> {
        let slot = self.slots.get_mut(&client)?;
        slot.wanted = false;
        slot.landed = WatchUpdate::Quiet;
        slot.watching.take().map(|(_, handle)| handle)
    }

    /// The consumer is gone: everything it held goes, its watch and any
    /// offer returned to let go of outside the caller's lock.
    pub fn forget(&mut self, client: C) -> [Option<H>; 2] {
        self.slots.remove(&client).map_or([None, None], |slot| {
            [
                slot.watching.map(|(_, handle)| handle),
                slot.offered.map(|(_, handle)| handle),
            ]
        })
    }

    /// Read `target` for `client` on the calling thread, there being no reader
    /// to hand it to: `read` is given the watch the consumer already holds
    /// there, to read through, and answers the listing with any watch it
    /// armed. The listing is taken as it lands, so one answered here follows
    /// its folder as one that waited does; answered with what taking it did
    /// to the watch.
    pub fn read_here(
        &mut self,
        client: C,
        target: &[String],
        read: impl FnOnce(Option<H>) -> (Result<Vec<Entry>, Errno>, Option<H>),
    ) -> (Result<Vec<Entry>, Errno>, Took<H>) {
        let (listed, armed) = read(self.relisting(client, target));
        let unwanted = self.offer_armed(listed.is_ok(), client, target, armed);
        let mut took = if listed.is_ok() {
            self.took(client, target)
        } else {
            Took::default()
        };
        if let Some(free) = took.release.iter_mut().find(|held| held.is_none()) {
            *free = unwanted;
        }
        (listed, took)
    }

    /// Drain through `drain`, on the calling thread, every watch a change was
    /// reported on, there being no reader to: whether anything landed for a
    /// consumer to take.
    pub fn drain_here(&mut self, mut drain: impl FnMut(&H) -> WatchUpdate) -> bool {
        let mut landed = false;
        while let Some((client, location, handle)) = self.next_drain() {
            let update = drain(&handle);
            landed |= self.deliver(client, &location, update);
        }
        landed
    }

    /// Stop recording, so a parked reader leaves.
    pub fn stop(&mut self) {
        self.stopping = true;
        for slot in self.slots.values_mut() {
            slot.wanted = false;
            slot.offered = None;
        }
    }
}

/// A directory a listing watches: the descriptor its watch is armed on, the
/// path its listing was spelled as, which a drained record's link resolves
/// against, and the wait-set it reports in once joined. Dropping it withdraws
/// the member before the descriptor closes, so the number is never reused
/// under a member still naming it; closing the descriptor ends the watch.
#[cfg(feature = "rt")]
pub struct WatchedDirectory {
    dir: tairix_rt::Dir,
    path: String,
    joined: core::sync::atomic::AtomicU64,
}

/// A watch not yet joined to a wait-set.
#[cfg(feature = "rt")]
const UNJOINED: u64 = u64::MAX;

/// A listing read through a descriptor armed first, so no change falls
/// between the two.
#[cfg(feature = "rt")]
pub struct WatchedListing {
    /// The listing.
    pub listed: Result<Vec<Entry>, Errno>,
    /// The armed directory, or why there is none: a listing that failed, or a
    /// watch the kernel refused — its `dir-watches` limit, or a volume that
    /// records no changes — which leaves the listing correct but not live.
    pub watch: Result<WatchedDirectory, Errno>,
}

#[cfg(feature = "rt")]
impl WatchedListing {
    /// Why the listing is not live, when that is worth saying: it listed, but
    /// its watch was refused for a reason other than the directory being one
    /// no volume records changes for, such as the namespace's own roots.
    #[must_use]
    pub fn unwatched(&self) -> Option<Errno> {
        match (&self.listed, &self.watch) {
            (Ok(_), Err(err)) if *err != Errno::NotImplemented => Some(*err),
            _ => None,
        }
    }
}

#[cfg(feature = "rt")]
impl WatchedDirectory {
    /// Read the listing of `target` a listing desk asked for: through
    /// `reuse`, the watch the consumer already holds there, so a reload keeps
    /// its pacing, or through a watch armed now at [`WATCH_LATENCY_NS`], which
    /// comes back to offer with the listing. `unwatched` hears why a folder
    /// that listed could not be watched.
    pub fn read(
        target: &[String],
        reuse: Option<alloc::sync::Arc<Self>>,
        unwatched: impl FnOnce(Errno),
    ) -> (Result<Vec<Entry>, Errno>, Option<alloc::sync::Arc<Self>>) {
        if let Some(dir) = reuse {
            return (dir.relist(), None);
        }
        let read = Self::list(target, WATCH_LATENCY_NS);
        if let Some(refused) = read.unwatched() {
            unwatched(refused);
        }
        (read.listed, read.watch.ok().map(alloc::sync::Arc::new))
    }

    /// List the directory named by root-first `components` through a
    /// descriptor armed with a watch paced at `latency_ns`.
    #[must_use]
    pub fn list(components: &[String], latency_ns: u64) -> WatchedListing {
        let path = match crate::vfs::absolute_path(components) {
            Ok(path) => path,
            Err(err) => {
                return WatchedListing {
                    listed: Err(err),
                    watch: Err(err),
                }
            }
        };
        let dir = match tairix_rt::open_dir(path.as_bytes()) {
            Ok(dir) => dir,
            Err(err) => {
                let err = Errno::from_syscall(err);
                return WatchedListing {
                    listed: Err(err),
                    watch: Err(err),
                };
            }
        };
        let armed = dir.watch(latency_ns).map_err(Errno::from_syscall);
        let listed = dir
            .read_all()
            .map_err(Errno::from_syscall)
            .and_then(|stream| {
                crate::vfs::entries_from_dir_stream(&path, &stream, &mut crate::RtLinkReader)
            });
        let watch = match (listed.as_ref().err(), armed) {
            (Some(&err), _) | (None, Err(err)) => Err(err),
            (None, Ok(())) => Ok(Self {
                dir,
                path,
                joined: core::sync::atomic::AtomicU64::new(UNJOINED),
            }),
        };
        WatchedListing { listed, watch }
    }

    /// The directory read again through the descriptor already armed, so a
    /// reload keeps its watch.
    ///
    /// # Errors
    ///
    /// The listing's refusal, as [`WatchedDirectory::list`] states it.
    pub fn relist(&self) -> Result<Vec<Entry>, Errno> {
        self.dir
            .read_all()
            .map_err(Errno::from_syscall)
            .and_then(|stream| {
                crate::vfs::entries_from_dir_stream(&self.path, &stream, &mut crate::RtLinkReader)
            })
    }

    /// Everything the watch has recorded, read through `scratch` until it
    /// holds no more. A batch that will not read or decode, or more than
    /// `DRAIN_BATCHES` of them, asks for the directory to be read whole — the
    /// read states the failure if it can no longer be listed, and no report is
    /// owed for what one drain left behind. A `scratch` too small for one
    /// change is read through one on the stack that holds it, so a drain
    /// always moves the watch on.
    #[must_use]
    pub fn drain(&self, scratch: &mut [u8]) -> WatchUpdate {
        let mut least = [0u8; DirChangeBatch::MIN_BUFFER];
        let scratch = if scratch.len() < DirChangeBatch::MIN_BUFFER {
            &mut least[..]
        } else {
            scratch
        };
        let mut update = WatchUpdate::Quiet;
        for _ in 0..DRAIN_BATCHES {
            let Some(bytes) = self
                .dir
                .read_changes(scratch)
                .ok()
                .and_then(|len| scratch.get(..len))
            else {
                update.absorb(WatchUpdate::Rescan);
                return update;
            };
            let Ok((batch, more)) = decode_batch(&self.path, bytes, &mut crate::RtLinkReader)
            else {
                update.absorb(WatchUpdate::Rescan);
                return update;
            };
            update.absorb(batch);
            if !more || matches!(update, WatchUpdate::Rescan | WatchUpdate::Gone) {
                return update;
            }
        }
        update.absorb(WatchUpdate::Rescan);
        update
    }
}

/// The most batches one drain reads: a directory changing faster than that
/// is read whole instead, so one watch never holds the reader.
#[cfg(feature = "rt")]
const DRAIN_BATCHES: usize = 16;

#[cfg(feature = "rt")]
impl WatchedDirectory {
    /// Report in wait-set `set` under `token`, once: a watch already joined
    /// stays where it is.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, as an [`Errno`]; the watch stays unjoined.
    pub fn join(&self, set: u64, token: u64) -> Result<(), Errno> {
        use core::sync::atomic::Ordering;
        if self
            .joined
            .compare_exchange(UNJOINED, set, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let ret = tairix_rt::waitset_ctl(
            set,
            tairix_abi::WaitSetOp::Add,
            tairix_abi::WaitSourceKind::DirWatch,
            u64::from(self.dir.fd()),
            token,
        );
        if ret != 0 {
            self.joined.store(UNJOINED, Ordering::Release);
            return Err(Errno::from_syscall(ret));
        }
        Ok(())
    }
}

#[cfg(feature = "rt")]
impl Drop for WatchedDirectory {
    fn drop(&mut self) {
        let set = *self.joined.get_mut();
        if set != UNJOINED {
            let _ = tairix_rt::waitset_ctl(
                set,
                tairix_abi::WaitSetOp::Del,
                tairix_abi::WaitSourceKind::DirWatch,
                u64::from(self.dir.fd()),
                0,
            );
        }
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
