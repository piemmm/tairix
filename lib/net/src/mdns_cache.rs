//! The per-interface multicast DNS record cache (RFC 6762 §10).
//!
//! Everything in here arrives unsolicited from an unauthenticated peer that
//! chooses its own arrival rate, so the cache is shaped by that rather than
//! by convenience.
//!
//! # Indexed, because a scan is a denial-of-service channel
//!
//! Every arriving record is found, renewed, retired, or placed in constant
//! time. A peer chooses the names, the rdata, and how many records it stacks
//! under one name, so each operation reaches only what it must:
//!
//! * finding a record walks the records identical in name, type and rdata —
//!   one, however many a peer piles under that name;
//! * a cache-flush walks the announcing source's own records at that name and
//!   type oldest first, takes each it retires off the walk for good, and stops
//!   at the first received within the last second, so it costs what it
//!   retires;
//! * a lookup walks the records at one name and type, freshest first, which
//!   is the answer, and a known-answer list stops at the first record past
//!   half its lifetime;
//! * a sweep reaches only the records whose next deadline has come, through a
//!   heap ordered by it.
//!
//! Every index is **keyed** with a per-boot secret the caller supplies, so a
//! peer cannot choose a set of names or rdata that lands in one chain.
//!
//! # Bounded, and fair within the bound
//!
//! [`super::MAX_RECORDS`] and [`super::MAX_RECORDS_PER_SOURCE`] are fixed
//! security bounds, never capacities that grow with the segment. A peer at
//! its own ceiling evicts its **own** oldest record, so shouting costs the
//! shouter its cache and nobody else theirs; only a globally full table
//! evicts by recency, whoever sent it.
//!
//! # Watched records
//!
//! Which question watches a record is the caller's to say — it holds the
//! questions — when it folds the record in and when a question starts or
//! stops. The cache keeps that question's id with the record, so every edge
//! it reports later names the question directly. Only a watched record arms
//! the RFC 6762 §5.2 refresh schedule, so a cache full of records nobody
//! asked about costs no timers.
//!
//! # Per-interface, and never merged
//!
//! One cache per interface, with no path between them. A record learned on
//! a coffee-shop network can never answer a question scoped to the wired
//! LAN, because the two caches do not know about each other.

use alloc::vec::Vec;
use core::hash::{BuildHasher, Hash, Hasher};

use tairix_abi::time::{Duration64, NANOS_PER_SEC};
use tairix_collections::{HashMap, IndexedHeap};
use tairix_hash::{BuildFastHash, BuildSipHash13, HashSeed};
use tairix_inline::{IntrusiveList, Link, LinkStore};

use crate::addr::IpAddr;
use crate::dns::{Name, RecordType};
use crate::timeutil::{nanos, NEVER};

use super::{QuestionId, Record, MAX_RECORDS, MAX_RECORDS_PER_SOURCE};

/// The fractions of a record's original TTL at which a querier with a live
/// interest re-asks, so the answer is refreshed before it expires (RFC 6762
/// §5.2).
const REFRESH_PERCENTS: [u32; 4] = [80, 85, 90, 95];

/// What happened to the cache when a record was folded in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Learned {
    /// A record the cache did not hold was added.
    Added,
    /// A record the cache already held had its lifetime renewed.
    Refreshed,
    /// A goodbye (TTL 0, RFC 6762 §10.1) retired a held record.
    Retired,
    /// The record was not taken: a goodbye for a record that was not held,
    /// or a record the cache had no room to index. What its cache-flush bit
    /// retired stays retired, and an eviction made to fit it stands.
    Ignored,
}

/// The keyed hash of an owner name and type, under one cache's secret: what
/// records are chained by and what a caller matches its questions against.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct RecordKey(u64);

/// A record with its [`RecordKey`], hashed once for every index that needs
/// it. Only [`RecordCache::keyed`] makes one, so the key always belongs to
/// the record.
#[derive(Clone, Copy, Debug)]
pub struct KeyedRecord<'r> {
    record: &'r Record,
    key: RecordKey,
}

impl<'r> KeyedRecord<'r> {
    /// The record.
    #[must_use]
    pub const fn record(&self) -> &'r Record {
        self.record
    }

    /// Its owner name and type's key.
    #[must_use]
    pub const fn key(&self) -> RecordKey {
        self.key
    }
}

/// One held record and the lifetime the cache tracks for it.
#[derive(Clone, Copy, Debug)]
pub struct CachedRecord {
    /// The record as received, with the TTL its sender gave it.
    pub record: Record,
    /// The address that sent it.
    pub source: IpAddr,
    /// When it was received.
    pub received: Duration64,
    /// When it expires.
    pub expires: Duration64,
}

/// A cache slot. `Option<Slot>` is the free/occupied distinction, so a freed
/// slot cannot be walked into: the link store answers `None` for it and
/// every list operation refuses the index.
#[derive(Clone, Debug)]
struct Slot {
    record: Record,
    source: IpAddr,
    key: RecordKey,
    /// The keyed hash of the whole record, which `find` is chained by.
    exact: u64,
    /// The keyed hash of the source and the owner name and type, which a
    /// cache-flush is chained by.
    owned: u64,
    received: u128,
    times: Times,
    /// How many of [`REFRESH_PERCENTS`] have passed.
    refresh_stage: u8,
    /// The question watching it.
    watcher: Option<QuestionId>,
    bucket_link: Link,
    exact_link: Link,
    owned_link: Link,
    source_link: Link,
    recency_link: Link,
}

impl CachedRecord {
    /// Whether a querier offers this record as a known answer at `now`: more
    /// than half its lifetime is left, so a responder would stay quiet for it
    /// (RFC 6762 §7.1).
    #[must_use]
    pub fn worth_offering(&self, now: Duration64) -> bool {
        offered(nanos(self.expires), self.record.ttl, nanos(now))
    }
}

impl Slot {
    fn cached(&self) -> CachedRecord {
        CachedRecord {
            record: self.record,
            source: self.source,
            received: crate::timeutil::from_nanos(self.received),
            expires: crate::timeutil::from_nanos(self.times.expires),
        }
    }
}

/// A held record's two deadlines; the cache's deadline heap holds the earlier.
#[derive(Clone, Copy, Debug)]
struct Times {
    expires: u128,
    /// The next re-query instant while a question watches the record, or
    /// [`NEVER`].
    refresh_at: u128,
}

impl Times {
    /// A record's deadlines from the instant `now_ns` it arrived with `ttl`,
    /// its refresh schedule armed only if a question watches it.
    fn fresh(now_ns: u128, ttl: u32, watched: bool) -> Self {
        let expires = expires_at(now_ns, ttl);
        Self {
            expires,
            refresh_at: if watched {
                refresh_instant(now_ns, expires, 0)
            } else {
                NEVER
            },
        }
    }

    const fn next(self) -> u128 {
        if self.expires < self.refresh_at {
            self.expires
        } else {
            self.refresh_at
        }
    }
}

/// A view of the slot array through one of its link fields.
///
/// A [`Link`] carries the node's on-a-list state inside it, so a node can be
/// on only one list per link; several lists thread the same slots through
/// several fields, and each gets its own store view.
macro_rules! link_view {
    ($name:ident, $field:ident) => {
        struct $name<'a>(&'a mut [Option<Slot>]);

        impl LinkStore for $name<'_> {
            fn link(&self, index: usize) -> Option<&Link> {
                self.0.get(index)?.as_ref().map(|slot| &slot.$field)
            }

            fn link_mut(&mut self, index: usize) -> Option<&mut Link> {
                self.0.get_mut(index)?.as_mut().map(|slot| &mut slot.$field)
            }
        }
    };
}

link_view!(BucketView, bucket_link);
link_view!(ExactView, exact_link);
link_view!(OwnedView, owned_link);
link_view!(SourceView, source_link);
link_view!(RecencyView, recency_link);

/// The chain heads a slot is threaded onto, so a placement can create every
/// missing one once it has its slot and a removal can drop every unused one.
#[derive(Clone, Copy)]
struct Heads {
    key: RecordKey,
    exact: u64,
    owned: u64,
    source: IpAddr,
}

/// One source's records at one name and type, newest first.
///
/// A flush takes a record off the chain for good while it lives out its last
/// second, so the chain cannot say when its head is unused: `holders` counts
/// every slot that names it, on the chain or not.
#[derive(Debug, Default)]
struct Owned {
    chain: IntrusiveList,
    holders: usize,
}

/// The bounded, indexed record cache of one interface.
#[derive(Debug)]
pub struct RecordCache {
    slots: Vec<Option<Slot>>,
    /// Indices of slots that hold nothing, as a stack.
    free: Vec<u32>,
    /// Chain head per owner name and type.
    buckets: HashMap<RecordKey, IntrusiveList, BuildFastHash>,
    /// Chain head per whole record.
    exact: HashMap<u64, IntrusiveList, BuildFastHash>,
    /// Chain head per source, owner name and type.
    owned: HashMap<u64, Owned, BuildFastHash>,
    /// Chain head per source address, which is also that source's count.
    sources: HashMap<IpAddr, IntrusiveList, BuildSipHash13>,
    /// Most-recently touched at the front; the back is the eviction
    /// candidate.
    recency: IntrusiveList,
    /// Keys every index; a peer chooses what is hashed under it.
    index_hash: BuildSipHash13,
    /// Every held slot at the earlier of its deadlines.
    deadlines: IndexedHeap<u128>,
    live: usize,
    /// Slots a find or a cache-flush has visited: the work counter the tests
    /// hold the per-record paths to.
    #[cfg(test)]
    walked: core::cell::Cell<usize>,
    /// Slots a sweep has handled, which the tests hold to what was due.
    #[cfg(test)]
    swept: core::cell::Cell<usize>,
    /// Slots a lookup has visited.
    #[cfg(test)]
    looked: core::cell::Cell<usize>,
}

impl RecordCache {
    /// An empty cache whose indexes are keyed with `key`.
    ///
    /// The key must be the per-boot secret wherever a peer can choose the
    /// names cached: [`HashSeed::UNKEYED`] names the predictable choice and
    /// is only for a test.
    #[must_use]
    pub fn new(key: HashSeed) -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            buckets: HashMap::with_hasher(BuildFastHash::new()),
            exact: HashMap::with_hasher(BuildFastHash::new()),
            owned: HashMap::with_hasher(BuildFastHash::new()),
            sources: HashMap::with_hasher(BuildSipHash13::with_seed(key)),
            recency: IntrusiveList::new(),
            index_hash: BuildSipHash13::with_seed(key),
            deadlines: IndexedHeap::new(),
            live: 0,
            #[cfg(test)]
            walked: core::cell::Cell::new(0),
            #[cfg(test)]
            swept: core::cell::Cell::new(0),
            #[cfg(test)]
            looked: core::cell::Cell::new(0),
        }
    }

    /// The slots visited since the last call.
    #[cfg(test)]
    pub(crate) fn take_walked(&self) -> usize {
        self.walked.take()
    }

    /// The slots swept since the last call.
    #[cfg(test)]
    pub(crate) fn take_swept(&self) -> usize {
        self.swept.take()
    }

    /// The slots lookups have visited since the last call.
    #[cfg(test)]
    pub(crate) fn take_looked(&self) -> usize {
        self.looked.take()
    }

    /// The number of records held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live
    }

    /// Whether the cache holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// The number of records attributable to `source`.
    #[must_use]
    pub fn len_from(&self, source: IpAddr) -> usize {
        self.sources.get(&source).map_or(0, IntrusiveList::len)
    }

    /// The next instant [`Self::advance`] has work to do, or `None`.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration64> {
        self.deadlines
            .peek()
            .map(|(_, at)| crate::timeutil::from_nanos(at))
    }

    /// The key records at `name` and `record_type` are chained by.
    #[must_use]
    pub fn key_of(&self, name: &Name, record_type: RecordType) -> RecordKey {
        let mut hasher = self.index_hash.build_hasher();
        name.hash(&mut hasher);
        hasher.write_u16(record_type.value());
        RecordKey(hasher.finish())
    }

    /// `record` with its key, for [`Self::learn`] and for the caller's own
    /// match against its questions.
    #[must_use]
    pub fn keyed<'r>(&self, record: &'r Record) -> KeyedRecord<'r> {
        KeyedRecord {
            record,
            key: self.key_of(&record.name, record.record_type()),
        }
    }

    /// Fold a received record into the cache. `watcher` is the live question
    /// asking its name and type, if one is.
    ///
    /// A TTL of zero is a goodbye (RFC 6762 §10.1): the record is held for
    /// one further second rather than dropped at once, so a peer that
    /// withdraws a name and immediately re-announces it does not leave every
    /// querier with a gap. The cache-flush bit (RFC 6762 §10.2) retires every
    /// *other* record this source holds at the same name and type, which is
    /// how a unique record replaces rather than accumulates.
    ///
    /// `gone` is told of a watched record the bounds evicted to make room,
    /// with its watcher, so a question never goes on believing in a record
    /// the cache no longer holds.
    pub fn learn(
        &mut self,
        now: Duration64,
        keyed: KeyedRecord<'_>,
        source: IpAddr,
        watcher: Option<QuestionId>,
        gone: &mut dyn FnMut(QuestionId, &CachedRecord),
    ) -> Learned {
        let now_ns = nanos(now);
        let record = keyed.record;
        let exact = self.exact_of(keyed);

        if record.ttl == 0 {
            return self.retire(now_ns, exact, record, source);
        }

        let mut owned = None;
        if record.cache_flush {
            let mine = self.owned_of(keyed.key, source);
            self.flush_others(now_ns, mine, record, source);
            owned = Some(mine);
        }

        if let Some(index) = self.find(exact, record) {
            self.renew(index, now_ns, record.ttl, watcher);
            return Learned::Refreshed;
        }

        let heads = Heads {
            key: keyed.key,
            exact,
            owned: owned.unwrap_or_else(|| self.owned_of(keyed.key, source)),
            source,
        };
        // Room in every index first, so the heads below cannot fail after an
        // eviction; the heads only after it, because evicting a chain's last
        // record drops that chain's head.
        if !self.reserve_index_room() {
            return Learned::Ignored;
        }
        let Some(index) = self.allocate(source, gone) else {
            return Learned::Ignored;
        };
        if !self.ensure_heads(heads) {
            self.release_slot(index);
            return Learned::Ignored;
        }
        if !self.place(index, heads, now_ns, record, watcher) {
            self.release_slot(index);
            return Learned::Ignored;
        }
        Learned::Added
    }

    /// Every held record answering `name`/`record_type`, whose
    /// [`Self::key_of`] is `key`, freshest first: most recently received, and
    /// a record whose lifetime a goodbye or a flush cut short last.
    pub fn lookup(&self, key: RecordKey, name: &Name, record_type: RecordType) -> Matches<'_> {
        Matches {
            slots: &self.slots,
            next: self.buckets.get(&key).and_then(IntrusiveList::front),
            name: *name,
            record_type,
            #[cfg(test)]
            looked: &self.looked,
        }
    }

    /// [`Self::lookup`] taken up again at a [`Matches::position`], for a walk
    /// resumed while the cache stands unchanged. From anywhere else it still
    /// yields only records answering `name`/`record_type`.
    pub(super) fn lookup_from(
        &self,
        at: usize,
        name: &Name,
        record_type: RecordType,
    ) -> Matches<'_> {
        Matches {
            slots: &self.slots,
            next: Some(at),
            name: *name,
            record_type,
            #[cfg(test)]
            looked: &self.looked,
        }
    }

    /// Whether a query of ours would carry `keyed` as a known answer: the
    /// cache holds an identical record (ignoring TTL and the cache-flush bit)
    /// [worth offering](CachedRecord::worth_offering) at `now`.
    #[must_use]
    pub fn would_offer(&self, now: Duration64, keyed: KeyedRecord<'_>) -> bool {
        self.find(self.exact_of(keyed), keyed.record)
            .and_then(|index| self.slots.get(index)?.as_ref())
            .is_some_and(|held| offered(held.times.expires, held.record.ttl, nanos(now)))
    }

    /// Name the question now asking `name`/`record_type`, whose
    /// [`Self::key_of`] is `key`, or `None` once none does, which arms or disarms the refresh schedule of the records held
    /// there; records learned later take the caller's word through
    /// [`Self::learn`].
    pub fn set_watched(
        &mut self,
        key: RecordKey,
        name: &Name,
        record_type: RecordType,
        watcher: Option<QuestionId>,
    ) {
        let mut next = self.buckets.get(&key).and_then(IntrusiveList::front);
        while let Some(index) = next {
            let Some(Some(slot)) = self.slots.get_mut(index) else {
                break;
            };
            next = slot.bucket_link.next();
            if slot.record.name != *name || slot.record.record_type() != record_type {
                continue;
            }
            slot.watcher = watcher;
            slot.times.refresh_at = if watcher.is_some() {
                refresh_instant(slot.received, slot.times.expires, slot.refresh_stage)
            } else {
                NEVER
            };
            self.deadlines.set(index, slot.times.next());
        }
    }

    /// Expire what `now` has reached and report, by watcher, which watched
    /// records are due a refreshing re-query.
    ///
    /// A record that expires is dropped rather than refreshed, so a caller
    /// never re-asks about something it has just forgotten; one whose
    /// refresh points have passed is reported once, however many passed.
    /// `gone` is told of each watched record that expired.
    pub fn advance(
        &mut self,
        now: Duration64,
        due: &mut dyn FnMut(QuestionId),
        gone: &mut dyn FnMut(QuestionId, &CachedRecord),
    ) {
        let now_ns = nanos(now);
        while let Some((index, at)) = self.deadlines.peek() {
            if at > now_ns {
                break;
            }
            #[cfg(test)]
            self.swept.set(self.swept.get() + 1);
            match self.slots.get_mut(index) {
                Some(Some(slot)) if slot.times.expires > now_ns => {
                    while slot.times.refresh_at <= now_ns {
                        slot.refresh_stage = slot.refresh_stage.saturating_add(1);
                        slot.times.refresh_at =
                            refresh_instant(slot.received, slot.times.expires, slot.refresh_stage);
                    }
                    if let Some(watcher) = slot.watcher {
                        due(watcher);
                    }
                    self.deadlines.set(index, slot.times.next());
                }
                Some(Some(_)) => self.remove_reporting(index, gone),
                _ => {
                    self.deadlines.remove(index);
                }
            }
        }
    }

    /// Drop every held record — what an interface going down does, since a
    /// record learned on a link is meaningless once that link is gone.
    /// `gone` is told of each watched record dropped.
    pub fn clear(&mut self, gone: &mut dyn FnMut(QuestionId, &CachedRecord)) {
        for slot in self.slots.iter().flatten() {
            if let Some(watcher) = slot.watcher {
                gone(watcher, &slot.cached());
            }
        }
        self.slots.clear();
        self.free.clear();
        self.buckets.clear();
        self.exact.clear();
        self.owned.clear();
        self.sources.clear();
        self.recency = IntrusiveList::new();
        self.deadlines.clear();
        self.live = 0;
    }

    /// The keyed hash of a whole record: its owner name and type, then its
    /// rdata.
    fn exact_of(&self, keyed: KeyedRecord<'_>) -> u64 {
        let mut hasher = self.index_hash.build_hasher();
        hasher.write_u64(keyed.key.0);
        keyed.record.data.hash(&mut hasher);
        hasher.finish()
    }

    /// The keyed hash of a source and an owner name and type.
    fn owned_of(&self, key: RecordKey, source: IpAddr) -> u64 {
        let mut hasher = self.index_hash.build_hasher();
        hasher.write_u64(key.0);
        source.hash(&mut hasher);
        hasher.finish()
    }

    /// The slot holding a record identical to `record`, or `None`.
    fn find(&self, exact: u64, record: &Record) -> Option<usize> {
        let mut next = self.exact.get(&exact).and_then(IntrusiveList::front);
        while let Some(index) = next {
            #[cfg(test)]
            self.walked.set(self.walked.get() + 1);
            let slot = self.slots.get(index)?.as_ref()?;
            if slot.record.same_record(record) {
                return Some(index);
            }
            next = slot.exact_link.next();
        }
        None
    }

    /// Retire a record a goodbye withdrew, keeping it for the one further
    /// second RFC 6762 §10.1 asks for.
    fn retire(&mut self, now_ns: u128, exact: u64, record: &Record, source: IpAddr) -> Learned {
        let Some(index) = self.find(exact, record) else {
            return Learned::Ignored;
        };
        let Some(Some(slot)) = self.slots.get_mut(index) else {
            return Learned::Ignored;
        };
        // Only the source that asserted a record may withdraw it; otherwise
        // any peer on the segment could delete a neighbour's advertisement.
        if slot.source != source {
            return Learned::Ignored;
        }
        slot.times.expires = slot
            .times
            .expires
            .min(now_ns.saturating_add(u128::from(NANOS_PER_SEC)));
        slot.times.refresh_at = NEVER;
        let key = slot.key;
        self.deadlines.set(index, slot.times.next());
        if let Some(bucket) = self.buckets.get_mut(&key) {
            let _ = bucket.move_to_back(&mut BucketView(&mut self.slots), index);
        }
        Learned::Retired
    }

    /// Retire the announcing source's other records at this name and type,
    /// which is what the cache-flush bit asks for.
    ///
    /// Scoped to the announcing source on purpose: the bit asserts sole
    /// ownership, and honouring it across sources would let any peer flush a
    /// neighbour's records by claiming their name. Each record is flushed
    /// once and taken off the chain, and the walk starts from the oldest and
    /// stops at the first record received within the last second, so a flush
    /// costs what it retires.
    fn flush_others(&mut self, now_ns: u128, owned: u64, record: &Record, source: IpAddr) {
        let one_second = u128::from(NANOS_PER_SEC);
        let flush_at = now_ns.saturating_add(one_second);
        let Self {
            slots,
            owned: chains,
            buckets,
            deadlines,
            ..
        } = self;
        let Some(chain) = chains.get_mut(&owned).map(|owned| &mut owned.chain) else {
            return;
        };
        let mut next = chain.back();
        while let Some(index) = next {
            #[cfg(test)]
            self.walked.set(self.walked.get() + 1);
            let Some(Some(slot)) = slots.get_mut(index) else {
                break;
            };
            // Everything nearer the front arrived later still. An announcement
            // of several records at one name arrives as several messages, and
            // flushing on the first would delete the rest.
            if now_ns.saturating_sub(slot.received) < one_second {
                break;
            }
            next = slot.owned_link.prev();
            if slot.source != source
                || slot.record.name != record.name
                || slot.record.record_type() != record.record_type()
                || slot.record.data == record.data
            {
                continue;
            }
            slot.times.expires = slot.times.expires.min(flush_at);
            slot.times.refresh_at = NEVER;
            let key = slot.key;
            deadlines.set(index, slot.times.next());
            let _ = chain.unlink(&mut OwnedView(slots), index);
            if let Some(bucket) = buckets.get_mut(&key) {
                let _ = bucket.move_to_back(&mut BucketView(slots), index);
            }
        }
    }

    /// Renew a held record's lifetime and restart its refresh schedule. It is
    /// the newest arrival again, so it goes to the front of every chain kept
    /// in arrival order, back onto its source's if a flush took it off.
    fn renew(&mut self, index: usize, now_ns: u128, ttl: u32, watcher: Option<QuestionId>) {
        let Some(Some(slot)) = self.slots.get_mut(index) else {
            return;
        };
        slot.received = now_ns;
        slot.record.ttl = ttl;
        slot.refresh_stage = 0;
        slot.watcher = watcher;
        slot.times = Times::fresh(now_ns, ttl, watcher.is_some());
        let (key, owned, flushed) = (slot.key, slot.owned, !slot.owned_link.is_linked());
        self.deadlines.set(index, slot.times.next());
        if let Some(owned) = self.owned.get_mut(&owned) {
            let view = &mut OwnedView(&mut self.slots);
            let _ = if flushed {
                owned.chain.push_front(view, index)
            } else {
                owned.chain.move_to_front(view, index)
            };
        }
        if let Some(bucket) = self.buckets.get_mut(&key) {
            let _ = bucket.move_to_front(&mut BucketView(&mut self.slots), index);
        }
        let _ = self
            .recency
            .move_to_front(&mut RecencyView(&mut self.slots), index);
    }

    /// A slot for a record from `source`, evicting under the bounds if one
    /// is needed, or `None` when the cache refuses the record.
    fn allocate(
        &mut self,
        source: IpAddr,
        gone: &mut dyn FnMut(QuestionId, &CachedRecord),
    ) -> Option<usize> {
        if self.len_from(source) >= MAX_RECORDS_PER_SOURCE {
            // The shouter pays: its own oldest record goes, never a
            // neighbour's.
            let oldest = self.sources.get(&source).and_then(IntrusiveList::back)?;
            self.remove_reporting(oldest, gone);
        } else if self.live >= MAX_RECORDS {
            let oldest = self.recency.back()?;
            self.remove_reporting(oldest, gone);
        }
        if let Some(index) = self.free.pop() {
            return Some(index as usize);
        }
        // The free stack gets room for every slot as the slot is made, so
        // giving one back can never fail and strand it.
        if self.slots.len() >= MAX_RECORDS
            || self.slots.try_reserve(1).is_err()
            || self.deadlines.try_reserve_slot(self.slots.len()).is_err()
            || self
                .free
                .try_reserve((self.slots.len() + 1).saturating_sub(self.free.len()))
                .is_err()
        {
            return None;
        }
        self.slots.push(None);
        Some(self.slots.len() - 1)
    }

    /// Fill an allocated slot and thread it onto every chain, or refuse —
    /// changing nothing — if its deadline cannot be held.
    fn place(
        &mut self,
        index: usize,
        heads: Heads,
        now_ns: u128,
        record: &Record,
        watcher: Option<QuestionId>,
    ) -> bool {
        let times = Times::fresh(now_ns, record.ttl, watcher.is_some());
        if self.deadlines.try_insert(index, times.next()).is_err() {
            return false;
        }
        self.slots[index] = Some(Slot {
            record: *record,
            source: heads.source,
            key: heads.key,
            exact: heads.exact,
            owned: heads.owned,
            received: now_ns,
            times,
            refresh_stage: 0,
            watcher,
            bucket_link: Link::UNLINKED,
            exact_link: Link::UNLINKED,
            owned_link: Link::UNLINKED,
            source_link: Link::UNLINKED,
            recency_link: Link::UNLINKED,
        });
        self.live += 1;

        if let Some(chain) = self.buckets.get_mut(&heads.key) {
            let _ = chain.push_front(&mut BucketView(&mut self.slots), index);
        }
        if let Some(chain) = self.exact.get_mut(&heads.exact) {
            let _ = chain.push_front(&mut ExactView(&mut self.slots), index);
        }
        if let Some(owned) = self.owned.get_mut(&heads.owned) {
            owned.holders += 1;
            let _ = owned
                .chain
                .push_front(&mut OwnedView(&mut self.slots), index);
        }
        if let Some(chain) = self.sources.get_mut(&heads.source) {
            let _ = chain.push_front(&mut SourceView(&mut self.slots), index);
        }
        let _ = self
            .recency
            .push_front(&mut RecencyView(&mut self.slots), index);
        true
    }

    /// Room for one more head in every index, changing nothing else.
    fn reserve_index_room(&mut self) -> bool {
        self.buckets.try_reserve(1).is_ok()
            && self.exact.try_reserve(1).is_ok()
            && self.owned.try_reserve(1).is_ok()
            && self.sources.try_reserve(1).is_ok()
    }

    /// Create every chain head `heads` names that does not exist. Room was
    /// reserved before any eviction, so no insert here can need to grow; a
    /// head left empty by a refusal regardless holds nothing, and a later
    /// record fills it or a clear drops it.
    fn ensure_heads(&mut self, heads: Heads) -> bool {
        ensure_head(&mut self.buckets, heads.key)
            && ensure_head(&mut self.exact, heads.exact)
            && ensure_head(&mut self.owned, heads.owned)
            && ensure_head(&mut self.sources, heads.source)
    }

    /// Hand back an allocated slot that was never filled.
    fn release_slot(&mut self, index: usize) {
        if let (Ok(index), Ok(())) = (u32::try_from(index), self.free.try_reserve(1)) {
            self.free.push(index);
        }
    }

    /// [`Self::remove`], telling `gone` first when a question was watching
    /// the record.
    fn remove_reporting(&mut self, index: usize, gone: &mut dyn FnMut(QuestionId, &CachedRecord)) {
        if let Some(Some(slot)) = self.slots.get(index) {
            if let Some(watcher) = slot.watcher {
                gone(watcher, &slot.cached());
            }
        }
        self.remove(index);
    }

    /// Unlink and free the slot at `index`.
    fn remove(&mut self, index: usize) {
        let Some(Some(slot)) = self.slots.get(index) else {
            return;
        };
        let heads = Heads {
            key: slot.key,
            exact: slot.exact,
            owned: slot.owned,
            source: slot.source,
        };
        unlink(
            &mut self.buckets,
            &heads.key,
            &mut BucketView(&mut self.slots),
            index,
        );
        unlink(
            &mut self.exact,
            &heads.exact,
            &mut ExactView(&mut self.slots),
            index,
        );
        if let Some(owned) = self.owned.get_mut(&heads.owned) {
            let _ = owned.chain.unlink(&mut OwnedView(&mut self.slots), index);
            owned.holders = owned.holders.saturating_sub(1);
            if owned.holders == 0 {
                self.owned.remove(&heads.owned);
            }
        }
        unlink(
            &mut self.sources,
            &heads.source,
            &mut SourceView(&mut self.slots),
            index,
        );
        let _ = self
            .recency
            .unlink(&mut RecencyView(&mut self.slots), index);
        self.slots[index] = None;
        self.deadlines.remove(index);
        self.live -= 1;
        if self.free.try_reserve(1).is_ok() {
            // The index fits: the slot array is bounded by MAX_RECORDS.
            if let Ok(index) = u32::try_from(index) {
                self.free.push(index);
            }
        }
    }
}

/// Make sure `map` has a head for `key`.
fn ensure_head<K: Eq + Hash, V: Default, S: BuildHasher>(
    map: &mut HashMap<K, V, S>,
    key: K,
) -> bool {
    map.contains_key(&key) || map.try_insert(key, V::default()).is_ok()
}

/// Take `index` off the chain `map` holds for `key`, dropping the head once
/// it is empty.
fn unlink<K: Eq + Hash, S: BuildHasher>(
    map: &mut HashMap<K, IntrusiveList, S>,
    key: &K,
    store: &mut impl LinkStore,
    index: usize,
) {
    if let Some(chain) = map.get_mut(key) {
        let _ = chain.unlink(store, index);
        if chain.is_empty() {
            map.remove(key);
        }
    }
}

/// Records the cache holds for one name and type.
#[derive(Clone, Debug)]
pub struct Matches<'a> {
    slots: &'a [Option<Slot>],
    next: Option<usize>,
    name: Name,
    record_type: RecordType,
    #[cfg(test)]
    looked: &'a core::cell::Cell<usize>,
}

impl Matches<'_> {
    /// Where the walk stands, which [`RecordCache::lookup_from`] resumes at.
    pub(super) const fn position(&self) -> Option<usize> {
        self.next
    }
}

impl Iterator for Matches<'_> {
    type Item = CachedRecord;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(index) = self.next {
            #[cfg(test)]
            self.looked.set(self.looked.get() + 1);
            let slot = self.slots.get(index)?.as_ref()?;
            self.next = slot.bucket_link.next();
            if slot.record.name != self.name || slot.record.record_type() != self.record_type {
                continue;
            }
            return Some(slot.cached());
        }
        None
    }
}

impl core::iter::FusedIterator for Matches<'_> {}

/// Whether a record expiring at `expires`, which arrived with `ttl` seconds,
/// has more than half its lifetime left at `now_ns`.
fn offered(expires: u128, ttl: u32, now_ns: u128) -> bool {
    expires.saturating_sub(now_ns).saturating_mul(2) > expires_at(0, ttl)
}

/// When a record received at `now_ns` with `ttl` seconds expires.
pub(super) fn expires_at(now_ns: u128, ttl: u32) -> u128 {
    now_ns.saturating_add(u128::from(ttl) * u128::from(NANOS_PER_SEC))
}

/// The instant a watched record's `stage`-th re-query is due, or [`NEVER`]
/// once every point has passed (RFC 6762 §5.2).
fn refresh_instant(received: u128, expires: u128, stage: u8) -> u128 {
    let Some(percent) = REFRESH_PERCENTS.get(usize::from(stage)) else {
        return NEVER;
    };
    let lifetime = expires.saturating_sub(received);
    received.saturating_add(lifetime * u128::from(*percent) / 100)
}

#[cfg(test)]
#[path = "mdns_cache_tests.rs"]
mod tests;
