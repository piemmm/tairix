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
//! * a cache-flush walks the records at that name and type not yet cut short,
//!   oldest first, takes each it retires off the walk for good, and stops at
//!   the first received within the last second, so it costs what it retires;
//! * a lookup walks the records at one name and type, freshest first, which
//!   is the answer; a record is moved behind the rest as it passes half its
//!   lifetime, so a known-answer list stops at the first such;
//! * a sweep reaches only the records whose next deadline has come, through a
//!   heap ordered by it.
//!
//! Every index is **keyed** with a per-boot secret the caller supplies, so a
//! peer cannot choose a set of names or rdata that lands in one chain.
//!
//! # Bounded, and fair within the bound
//!
//! [`super::MAX_RECORDS`], [`super::MAX_RECORDS_PER_KEY`] and
//! [`super::MAX_RECORDS_PER_SOURCE`] are fixed security bounds, never
//! capacities that grow with the segment. A name and type at its bound evicts
//! its own oldest record, so piling records under one name costs that name
//! and nobody else's. A record nobody asks about counts against its source's
//! bound however it came to be one — learned so, left so when its question
//! stops, or rescued from an owner's goodbye — and a source at its bound
//! evicts its own oldest such. A record may pay more than one bound at once,
//! and pays each with its own eviction. A full table evicts the least
//! recently used record nobody asked about before any record a question
//! watches, so however many addresses a peer forges, unsolicited records can
//! only displace each other.
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
//! # Whose a record is
//!
//! A record is attributed to the source that asserted it. Only that source
//! renews it on its own say or withdraws it; another source asserting the
//! same record can only extend its life, and one asserting a record its
//! owner withdrew in the last second rescues it and becomes its owner (RFC
//! 6762 §10.1). The cache-flush bit retires every other record at the name
//! and type, whoever asserted it, received over a second ago (RFC 6762
//! §10.2) — except on a pointer at a service type's name, which every
//! instance's responder shares and none can own.
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

use super::{
    QuestionId, RData, Record, MAX_RECORDS, MAX_RECORDS_PER_KEY, MAX_RECORDS_PER_SOURCE,
    TTL_HELD_MAX_SECS,
};

/// The fractions of a record's original TTL at which a querier with a live
/// interest re-asks, so the answer is refreshed before it expires (RFC 6762
/// §5.2).
const REFRESH_PERCENTS: [u32; 4] = [80, 85, 90, 95];

/// The spread added to each refresh point, as a percentage of the record's
/// lifetime (RFC 6762 §5.2), so queriers — and one querier's records — do
/// not all re-ask at once. A record's own share of it is taken from its keyed
/// hash: secret to the segment, and fixed for as long as the record is.
const REFRESH_SPREAD_PERCENT: u32 = 2;

/// What happened to the cache when a record was folded in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Learned {
    /// A record the cache did not hold was added.
    Added,
    /// A record the cache already held had its lifetime renewed.
    Refreshed,
    /// A record the cache held, asserted by another source for no longer
    /// than it already lives: nothing changed.
    Corroborated,
    /// A goodbye (TTL 0, RFC 6762 §10.1) retired a held record.
    Retired,
    /// The record was not taken: a goodbye for a record that was not held or
    /// not the sender's, or a record the cache had no room to index. What
    /// its cache-flush bit retired stays retired, and an eviction made to fit
    /// it stands.
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
    /// The record as received, its TTL no longer than
    /// [`super::TTL_HELD_MAX_SECS`].
    pub record: Record,
    /// The source it is attributed to.
    pub source: IpAddr,
    /// When it was last received from a source whose assertion counted.
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
    received: u128,
    times: Times,
    /// How many of [`REFRESH_PERCENTS`] have passed.
    refresh_stage: u8,
    /// The question watching it.
    watcher: Option<QuestionId>,
    bucket_link: Link,
    exact_link: Link,
    /// On its name and type's chain of records a flush may still retire:
    /// unlinked once a goodbye or a flush cuts it short.
    flush_link: Link,
    /// On its source's chain while no question watches it.
    source_link: Link,
    /// On the recency list of watched records, or of the rest.
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

    /// Whether a goodbye or a flush has cut its lifetime short.
    const fn cut(&self) -> bool {
        !self.flush_link.is_linked()
    }
}

/// A held record's deadlines; the cache's deadline heap holds the earlier.
#[derive(Clone, Copy, Debug)]
struct Times {
    expires: u128,
    /// The next re-query instant while a question watches the record, or
    /// [`NEVER`].
    refresh_at: u128,
}

impl Times {
    /// A record's deadlines from the instant `now_ns` it arrived with `ttl`,
    /// its refresh schedule armed, spread by `spread`, only if a question
    /// watches it.
    fn fresh(now_ns: u128, ttl: u32, watched: bool, spread: u64) -> Self {
        let expires = expires_at(now_ns, ttl);
        Self {
            expires,
            refresh_at: if watched {
                refresh_instant(now_ns, expires, 0, spread)
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

/// When a record expiring at `expires` with `ttl` seconds passes half its
/// lifetime, and is no longer offered as a known answer.
fn halfway(expires: u128, ttl: u32) -> u128 {
    expires.saturating_sub(expires_at(0, ttl) / 2)
}

/// A view of the slot array through one of their link fields.
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
link_view!(FlushView, flush_link);
link_view!(SourceView, source_link);
link_view!(RecencyView, recency_link);

/// The chain heads a slot is threaded onto, so a placement can create every
/// missing one once it has its slot and a removal can drop every unused one.
#[derive(Clone, Copy)]
struct Heads {
    key: RecordKey,
    exact: u64,
    source: IpAddr,
    /// Whether a question watches it, which keeps it off its source's chain.
    watched: bool,
}

/// The records at one name and type a flush may still retire, newest first.
///
/// A flush or a goodbye takes a record off the chain for good while it lives
/// out its last second, so the chain cannot say when its head is unused:
/// `holders` counts every slot at the name and type, on the chain or not.
#[derive(Debug, Default)]
struct Flushable {
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
    /// Per owner name and type, the records a flush may still retire.
    flushable: HashMap<RecordKey, Flushable, BuildFastHash>,
    /// Per source address, the records attributed to it that no question
    /// watches; also that source's count of them.
    sources: HashMap<IpAddr, IntrusiveList, BuildSipHash13>,
    /// Records a question watches, most recently touched at the front.
    watched: IntrusiveList,
    /// The rest, likewise: the first eviction candidates.
    unwatched: IntrusiveList,
    /// Keys every index; a peer chooses what is hashed under it.
    index_hash: BuildSipHash13,
    /// Every held slot at the earlier of its deadlines.
    deadlines: IndexedHeap<u128>,
    /// Every held slot not yet past half its lifetime, at the instant it
    /// passes it. Moving it behind the rest then wants no wake of its own:
    /// only a query reads the order, and every sweep drains what is due.
    halfways: IndexedHeap<u128>,
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
    /// Refuse every source chain an unwatched record would join, so the
    /// tests can drive the path running out of memory takes.
    #[cfg(test)]
    refuse_sources: bool,
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
            flushable: HashMap::with_hasher(BuildFastHash::new()),
            sources: HashMap::with_hasher(BuildSipHash13::with_seed(key)),
            watched: IntrusiveList::new(),
            unwatched: IntrusiveList::new(),
            index_hash: BuildSipHash13::with_seed(key),
            deadlines: IndexedHeap::new(),
            halfways: IndexedHeap::new(),
            live: 0,
            #[cfg(test)]
            walked: core::cell::Cell::new(0),
            #[cfg(test)]
            swept: core::cell::Cell::new(0),
            #[cfg(test)]
            looked: core::cell::Cell::new(0),
            #[cfg(test)]
            refuse_sources: false,
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

    /// Panic unless every chain, map, heap and count agrees with the slots
    /// and every bound holds: the oracle the fuzz harness holds the cache to.
    /// Each chain is walked once, so a check costs what the cache holds.
    #[cfg(any(test, feature = "test-support"))]
    pub fn assert_consistent(&self) {
        use alloc::collections::BTreeMap;
        let walked = Walked::of(self);
        let mut per_source: BTreeMap<IpAddr, usize> = BTreeMap::new();
        let mut holders: BTreeMap<u64, usize> = BTreeMap::new();
        let mut live = 0;
        for (index, slot) in self.slots.iter().enumerate() {
            let Some(slot) = slot else {
                assert!(walked.holds_nothing_at(index), "a free slot still linked");
                assert_eq!(self.deadlines.get(index), None, "a free slot's deadline");
                assert_eq!(self.halfways.get(index), None, "a free slot's half-life");
                continue;
            };
            live += 1;
            self.assert_slot(index, slot, &walked);
            *holders.entry(slot.key.0).or_default() += 1;
            if slot.watcher.is_none() {
                *per_source.entry(slot.source).or_default() += 1;
            }
        }
        assert_eq!(live, self.live, "the live count");
        assert!(self.live <= MAX_RECORDS);
        let mut free: Vec<u32> = self.free.clone();
        free.sort_unstable();
        free.dedup();
        assert_eq!(free.len(), self.free.len(), "a free slot listed twice");
        assert_eq!(free.len() + self.live, self.slots.len(), "a slot lost");
        for &index in &free {
            let index = usize::try_from(index).expect("slots are bounded");
            assert!(
                matches!(self.slots.get(index), Some(None)),
                "a live slot listed free"
            );
        }
        for (&source, &count) in &per_source {
            assert_eq!(self.len_from(source), count);
            assert!(count <= MAX_RECORDS_PER_SOURCE, "a source past its share");
        }
        for (key, flushable) in &self.flushable {
            assert_eq!(
                holders.get(&key.0).copied(),
                Some(flushable.holders),
                "flush holders"
            );
        }
        self.assert_offer_order();
        assert_eq!(
            self.watched.len() + self.unwatched.len(),
            self.live,
            "recency lists out of step"
        );
    }

    /// One held record's place on every chain and heap, against the walk.
    #[cfg(any(test, feature = "test-support"))]
    fn assert_slot(&self, index: usize, slot: &Slot, walked: &Walked) {
        let unwatched = slot.watcher.is_none();
        assert_eq!(walked.bucket[index], Some(slot.key), "its bucket");
        assert_eq!(walked.exact[index], Some(slot.exact), "its exact chain");
        assert_eq!(
            walked.flush[index],
            (!slot.cut()).then_some(slot.key),
            "its flush chain"
        );
        assert_eq!(
            walked.source[index],
            unwatched.then_some(slot.source),
            "counted to its source exactly when nobody watches it"
        );
        assert_eq!(walked.unwatched[index], Some(unwatched), "its recency list");
        assert_eq!(
            self.deadlines.get(index),
            Some(slot.times.next()),
            "its deadline"
        );
        assert!(
            !slot.cut() || slot.times.refresh_at == NEVER,
            "a cut record refreshes"
        );
        assert!(
            !unwatched || slot.times.refresh_at == NEVER,
            "an unwatched record refreshes"
        );
        assert!(
            !slot.cut() || self.halfways.get(index).is_none(),
            "a cut record reordered again"
        );
    }

    /// Every name within its bound, and what a known-answer list offers
    /// first in its chain: once one record has been moved behind, none
    /// after it is in front.
    #[cfg(any(test, feature = "test-support"))]
    fn assert_offer_order(&self) {
        for (key, bucket) in &self.buckets {
            assert!(bucket.len() <= MAX_RECORDS_PER_KEY, "a name past its share");
            let mut offered_ended = false;
            let mut next = bucket.front();
            while let Some(index) = next {
                let Some(Some(slot)) = self.slots.get(index) else {
                    break;
                };
                assert_eq!(slot.key, *key);
                let behind = slot.cut() || self.halfways.get(index).is_none();
                assert!(
                    !offered_ended || behind,
                    "an offered record behind one that is not"
                );
                offered_ended |= behind;
                next = slot.bucket_link.next();
            }
        }
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

    /// The number of records attributed to `source` that no question
    /// watches: what [`super::MAX_RECORDS_PER_SOURCE`] bounds.
    #[must_use]
    pub fn len_from(&self, source: IpAddr) -> usize {
        self.sources.get(&source).map_or(0, IntrusiveList::len)
    }

    /// The number of records held at the name and type `key` names: what
    /// [`super::MAX_RECORDS_PER_KEY`] bounds.
    #[must_use]
    pub fn len_at(&self, key: RecordKey) -> usize {
        self.buckets.get(&key).map_or(0, IntrusiveList::len)
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

    /// Fold a record received from `source` into the cache. `watcher` is the
    /// live question asking its name and type, if one is.
    ///
    /// A TTL past [`super::TTL_HELD_MAX_SECS`] is held as that. A TTL of zero
    /// is a goodbye (RFC 6762 §10.1), which only the record's own source may
    /// say: the record is held for one further second rather than dropped at
    /// once, so a peer that withdraws a name and immediately re-announces it
    /// does not leave every querier with a gap. A record already held is
    /// renewed by its own source, extended by another, and rescued from a
    /// goodbye or a flush in its last second by any. The cache-flush bit
    /// (RFC 6762 §10.2) retires every *other* record at the same name and
    /// type received over a second ago, which is how a unique record
    /// replaces rather than accumulates.
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
        let held = Record {
            ttl: keyed.record.ttl.min(TTL_HELD_MAX_SECS),
            ..*keyed.record
        };
        let record = &held;
        let exact = self.exact_of(keyed);

        if record.ttl == 0 {
            return self.retire(now_ns, exact, record, source);
        }
        if record.cache_flush && flush_claims(record) {
            self.flush_others(now_ns, keyed.key, record);
        }
        if let Some(index) = self.find(exact, record) {
            return self.reassert(index, now_ns, record, source, watcher);
        }

        let heads = Heads {
            key: keyed.key,
            exact,
            source,
            watched: watcher.is_some(),
        };
        // Room in every index first, so the heads below cannot fail after an
        // eviction; the heads only after it, because evicting a chain's last
        // record drops that chain's head.
        if !self.reserve_index_room() {
            return Learned::Ignored;
        }
        let Some(index) = self.allocate(heads, gone) else {
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
    /// [`Self::key_of`] is `key`, freshest first: most recently received,
    /// then those past half their lifetime, and those a goodbye or a flush
    /// cut short last.
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

    /// The keyed hash of the record slot `at` holds, which names that record
    /// for as long as it is held there.
    pub(super) fn exact_at(&self, at: usize) -> Option<u64> {
        Some(self.slots.get(at)?.as_ref()?.exact)
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

    /// The record identical to `keyed` (ignoring TTL and the cache-flush
    /// bit), as held.
    #[must_use]
    pub fn held(&self, keyed: KeyedRecord<'_>) -> Option<CachedRecord> {
        let index = self.find(self.exact_of(keyed), keyed.record)?;
        Some(self.slots.get(index)?.as_ref()?.cached())
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
    /// [`Self::key_of`] is `key`, or `None` once none does, which arms or
    /// disarms the refresh schedule of the records held there and moves them
    /// between the watched and the rest; records learned later take the
    /// caller's word through [`Self::learn`].
    pub fn set_watched(
        &mut self,
        key: RecordKey,
        name: &Name,
        record_type: RecordType,
        watcher: Option<QuestionId>,
    ) {
        let mut next = self.buckets.get(&key).and_then(IntrusiveList::front);
        while let Some(index) = next {
            let Some(Some(slot)) = self.slots.get(index) else {
                break;
            };
            next = slot.bucket_link.next();
            if slot.record.name != *name
                || slot.record.record_type() != record_type
                || slot.watcher == watcher
            {
                continue;
            }
            self.watch(index, watcher);
            // Room made for a record nobody watches may have taken the one
            // after it. A record refused its source's chain went too, and the
            // walk starts over, passing what it already moved.
            next = match self.slots.get(index) {
                Some(Some(slot)) => slot.bucket_link.next(),
                _ => self.buckets.get(&key).and_then(IntrusiveList::front),
            };
        }
    }

    /// Expire what `now` has reached, move behind the rest each record that
    /// has passed half its lifetime, and report, by watcher, which watched
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
        while let Some((index, at)) = self.halfways.peek() {
            if at > now_ns {
                break;
            }
            self.halfways.remove(index);
            let key = self
                .slots
                .get(index)
                .and_then(Option::as_ref)
                .map(|slot| slot.key);
            if let Some(bucket) = key.and_then(|key| self.buckets.get_mut(&key)) {
                let _ = bucket.move_to_back(&mut BucketView(&mut self.slots), index);
            }
        }
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
                        slot.times.refresh_at = refresh_instant(
                            slot.received,
                            slot.times.expires,
                            slot.refresh_stage,
                            slot.exact,
                        );
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
        self.flushable.clear();
        self.sources.clear();
        self.watched = IntrusiveList::new();
        self.unwatched = IntrusiveList::new();
        self.deadlines.clear();
        self.halfways.clear();
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

    /// `record`, held at `index`, asserted again by `source`: renewed by its
    /// own source, rescued by any from a goodbye or a flush, and otherwise
    /// only extended.
    fn reassert(
        &mut self,
        index: usize,
        now_ns: u128,
        record: &Record,
        source: IpAddr,
        watcher: Option<QuestionId>,
    ) -> Learned {
        let Some(Some(slot)) = self.slots.get(index) else {
            return Learned::Ignored;
        };
        if slot.source != source {
            if slot.cut() {
                // A record another responder still asserts outlives its
                // owner's goodbye (RFC 6762 §10.1), and is that responder's
                // from here.
                if !self.attribute(index, source) {
                    return Learned::Ignored;
                }
            } else if expires_at(now_ns, record.ttl) <= slot.times.expires {
                return Learned::Corroborated;
            }
        }
        self.renew(index, now_ns, record.ttl, watcher);
        Learned::Refreshed
    }

    /// Attribute the record at `index` to `source`, which a record nobody
    /// watches counts against; `false` when that source's chain cannot be
    /// made.
    fn attribute(&mut self, index: usize, source: IpAddr) -> bool {
        let Some(Some(slot)) = self.slots.get(index) else {
            return false;
        };
        let (from, watched) = (slot.source, slot.watcher.is_some());
        if !watched {
            if !self.ready_source(source) {
                return false;
            }
            unlink(
                &mut self.sources,
                &from,
                &mut SourceView(&mut self.slots),
                index,
            );
        }
        if let Some(Some(slot)) = self.slots.get_mut(index) {
            slot.source = source;
        }
        if !watched {
            if let Some(chain) = self.sources.get_mut(&source) {
                let _ = chain.push_front(&mut SourceView(&mut self.slots), index);
            }
        }
        true
    }

    /// Retire a record its own source's goodbye withdrew, keeping it for the
    /// one further second RFC 6762 §10.1 asks for.
    fn retire(&mut self, now_ns: u128, exact: u64, record: &Record, source: IpAddr) -> Learned {
        let Some(index) = self.find(exact, record) else {
            return Learned::Ignored;
        };
        let Some(Some(slot)) = self.slots.get(index) else {
            return Learned::Ignored;
        };
        // Only the source that asserted a record may withdraw it; otherwise
        // any peer on the segment could delete a neighbour's advertisement.
        if slot.source != source {
            return Learned::Ignored;
        }
        self.cut_short(index, now_ns);
        Learned::Retired
    }

    /// Give the record at `index` at most one more second, off the flush
    /// chain, behind every record at its name and type, and refreshed no
    /// more.
    fn cut_short(&mut self, index: usize, now_ns: u128) {
        let Some(Some(slot)) = self.slots.get_mut(index) else {
            return;
        };
        slot.times.expires = slot
            .times
            .expires
            .min(now_ns.saturating_add(u128::from(NANOS_PER_SEC)));
        slot.times.refresh_at = NEVER;
        let key = slot.key;
        self.deadlines.set(index, slot.times.next());
        self.halfways.remove(index);
        if let Some(flushable) = self.flushable.get_mut(&key) {
            let _ = flushable
                .chain
                .unlink(&mut FlushView(&mut self.slots), index);
        }
        if let Some(bucket) = self.buckets.get_mut(&key) {
            let _ = bucket.move_to_back(&mut BucketView(&mut self.slots), index);
        }
    }

    /// Retire every other record at `record`'s name and type, whoever
    /// asserted it, which is what the cache-flush bit asks for.
    ///
    /// The walk starts from the oldest and stops at the first record received
    /// within the last second, and each record is flushed once and taken off
    /// the chain, so a flush costs what it retires. An announcement of
    /// several records at one name arrives as several messages, and the
    /// second's grace is what keeps the first from flushing the rest.
    fn flush_others(&mut self, now_ns: u128, key: RecordKey, record: &Record) {
        let one_second = u128::from(NANOS_PER_SEC);
        let mut next = self
            .flushable
            .get(&key)
            .and_then(|flushable| flushable.chain.back());
        while let Some(index) = next {
            #[cfg(test)]
            self.walked.set(self.walked.get() + 1);
            let Some(Some(slot)) = self.slots.get(index) else {
                break;
            };
            // Everything nearer the front arrived later still.
            if now_ns.saturating_sub(slot.received) < one_second {
                break;
            }
            next = slot.flush_link.prev();
            if slot.record.name != record.name
                || slot.record.record_type() != record.record_type()
                || slot.record.data == record.data
            {
                continue;
            }
            self.cut_short(index, now_ns);
        }
    }

    /// Renew a held record's lifetime and restart its refresh schedule. It is
    /// the newest arrival again, so it goes to the front of every chain kept
    /// in arrival order, back onto the flush chain if it was cut short.
    fn renew(&mut self, index: usize, now_ns: u128, ttl: u32, watcher: Option<QuestionId>) {
        self.watch(index, watcher);
        let Some(Some(slot)) = self.slots.get_mut(index) else {
            return;
        };
        slot.received = now_ns;
        slot.record.ttl = ttl;
        slot.refresh_stage = 0;
        slot.times = Times::fresh(now_ns, ttl, watcher.is_some(), slot.exact);
        let (key, cut) = (slot.key, slot.cut());
        let half = halfway(slot.times.expires, ttl);
        self.deadlines.set(index, slot.times.next());
        // Its place was reserved with the slot, so it cannot fail.
        let _ = self.halfways.try_insert(index, half);
        if let Some(flushable) = self.flushable.get_mut(&key) {
            let view = &mut FlushView(&mut self.slots);
            let _ = if cut {
                flushable.chain.push_front(view, index)
            } else {
                flushable.chain.move_to_front(view, index)
            };
        }
        if let Some(bucket) = self.buckets.get_mut(&key) {
            let _ = bucket.move_to_front(&mut BucketView(&mut self.slots), index);
        }
        let recency = if watcher.is_some() {
            &mut self.watched
        } else {
            &mut self.unwatched
        };
        let _ = recency.move_to_front(&mut RecencyView(&mut self.slots), index);
    }

    /// Have the record at `index` watched by `watcher`, or by nobody: its
    /// refresh schedule armed or disarmed — never for a record cut short —
    /// and it moved between the watched and the rest. A record that cannot
    /// be counted against its source once nobody watches it is not kept.
    fn watch(&mut self, index: usize, watcher: Option<QuestionId>) {
        let Some(Some(slot)) = self.slots.get(index) else {
            return;
        };
        let (was, now, source) = (slot.watcher.is_some(), watcher.is_some(), slot.source);
        if was && !now && !self.ready_source(source) {
            self.remove(index);
            return;
        }
        let Some(Some(slot)) = self.slots.get_mut(index) else {
            return;
        };
        slot.watcher = watcher;
        slot.times.refresh_at = if watcher.is_some() && !slot.cut() {
            refresh_instant(
                slot.received,
                slot.times.expires,
                slot.refresh_stage,
                slot.exact,
            )
        } else {
            NEVER
        };
        self.deadlines.set(index, slot.times.next());
        if was == now {
            return;
        }
        let view = &mut RecencyView(&mut self.slots);
        let (from, to) = if now {
            (&mut self.unwatched, &mut self.watched)
        } else {
            (&mut self.watched, &mut self.unwatched)
        };
        let _ = from.unlink(view, index);
        let _ = to.push_front(view, index);
        if now {
            unlink(
                &mut self.sources,
                &source,
                &mut SourceView(&mut self.slots),
                index,
            );
        } else if let Some(chain) = self.sources.get_mut(&source) {
            let _ = chain.push_front(&mut SourceView(&mut self.slots), index);
        }
    }

    /// Make room for one more unwatched record from `source`: the shouter
    /// pays, its own oldest such record going. No question watches the one
    /// evicted, so there is nobody to tell.
    fn make_room_from(&mut self, source: IpAddr) {
        if self.len_from(source) >= MAX_RECORDS_PER_SOURCE {
            if let Some(oldest) = self.sources.get(&source).and_then(IntrusiveList::back) {
                self.remove(oldest);
            }
        }
    }

    /// Ready `source`'s chain to take one more unwatched record, made room
    /// on; `false` when the chain cannot be made.
    fn ready_source(&mut self, source: IpAddr) -> bool {
        #[cfg(test)]
        if self.refuse_sources {
            return false;
        }
        // Room for the head before the eviction, the head after it: evicting
        // a chain's last record drops that chain's head.
        if self.sources.try_reserve(1).is_err() {
            return false;
        }
        self.make_room_from(source);
        ensure_head(&mut self.sources, source)
    }

    /// A slot for a record placed as `heads` says, evicting under the bounds
    /// if one is needed, or `None` when the cache refuses the record.
    fn allocate(
        &mut self,
        heads: Heads,
        gone: &mut dyn FnMut(QuestionId, &CachedRecord),
    ) -> Option<usize> {
        // Every bound is held, each by its own eviction.
        if self.len_at(heads.key) >= MAX_RECORDS_PER_KEY {
            // The name pays for what is piled under it, never another name.
            let oldest = self.buckets.get(&heads.key).and_then(IntrusiveList::back)?;
            self.remove_reporting(oldest, gone);
        }
        if !heads.watched {
            self.make_room_from(heads.source);
        }
        if self.live >= MAX_RECORDS {
            let oldest = self.unwatched.back().or_else(|| self.watched.back())?;
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
            || self.halfways.try_reserve_slot(self.slots.len()).is_err()
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
        let times = Times::fresh(now_ns, record.ttl, watcher.is_some(), heads.exact);
        if self.deadlines.try_insert(index, times.next()).is_err()
            || self
                .halfways
                .try_insert(index, halfway(times.expires, record.ttl))
                .is_err()
        {
            self.deadlines.remove(index);
            return false;
        }
        self.slots[index] = Some(Slot {
            record: *record,
            source: heads.source,
            key: heads.key,
            exact: heads.exact,
            received: now_ns,
            times,
            refresh_stage: 0,
            watcher,
            bucket_link: Link::UNLINKED,
            exact_link: Link::UNLINKED,
            flush_link: Link::UNLINKED,
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
        if let Some(flushable) = self.flushable.get_mut(&heads.key) {
            flushable.holders += 1;
            let _ = flushable
                .chain
                .push_front(&mut FlushView(&mut self.slots), index);
        }
        if !heads.watched {
            if let Some(chain) = self.sources.get_mut(&heads.source) {
                let _ = chain.push_front(&mut SourceView(&mut self.slots), index);
            }
        }
        let recency = if heads.watched {
            &mut self.watched
        } else {
            &mut self.unwatched
        };
        let _ = recency.push_front(&mut RecencyView(&mut self.slots), index);
        true
    }

    /// Room for one more head in every index, changing nothing else.
    fn reserve_index_room(&mut self) -> bool {
        self.buckets.try_reserve(1).is_ok()
            && self.exact.try_reserve(1).is_ok()
            && self.flushable.try_reserve(1).is_ok()
            && self.sources.try_reserve(1).is_ok()
    }

    /// Create every chain head `heads` names that does not exist. Room was
    /// reserved before any eviction, so no insert here can need to grow; a
    /// head left empty by a refusal regardless holds nothing, and a later
    /// record fills it or a clear drops it.
    fn ensure_heads(&mut self, heads: Heads) -> bool {
        ensure_head(&mut self.buckets, heads.key)
            && ensure_head(&mut self.exact, heads.exact)
            && ensure_head(&mut self.flushable, heads.key)
            && (heads.watched || ensure_head(&mut self.sources, heads.source))
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
        let (key, exact, source, watched) =
            (slot.key, slot.exact, slot.source, slot.watcher.is_some());
        unlink(
            &mut self.buckets,
            &key,
            &mut BucketView(&mut self.slots),
            index,
        );
        unlink(
            &mut self.exact,
            &exact,
            &mut ExactView(&mut self.slots),
            index,
        );
        if let Some(flushable) = self.flushable.get_mut(&key) {
            let _ = flushable
                .chain
                .unlink(&mut FlushView(&mut self.slots), index);
            flushable.holders = flushable.holders.saturating_sub(1);
            if flushable.holders == 0 {
                self.flushable.remove(&key);
            }
        }
        if !watched {
            unlink(
                &mut self.sources,
                &source,
                &mut SourceView(&mut self.slots),
                index,
            );
        }
        let recency = if watched {
            &mut self.watched
        } else {
            &mut self.unwatched
        };
        let _ = recency.unlink(&mut RecencyView(&mut self.slots), index);
        self.slots[index] = None;
        self.deadlines.remove(index);
        self.halfways.remove(index);
        self.live -= 1;
        if self.free.try_reserve(1).is_ok() {
            // The index fits: the slot array is bounded by MAX_RECORDS.
            if let Ok(index) = u32::try_from(index) {
                self.free.push(index);
            }
        }
    }
}

/// Whether the cache-flush bit on `record` claims the name and type alone. A
/// pointer at a service type's name — `_ipp._tcp.local`, the type
/// enumeration — is shared by every instance's responder (RFC 6763 §4.1), so
/// a flush bit on one is a responder's mistake, never a claim to honour.
fn flush_claims(record: &Record) -> bool {
    let shared_pointer = matches!(record.data, RData::Ptr(_))
        && record
            .name
            .labels()
            .next()
            .is_some_and(|label| label.starts_with(b"_"));
    !shared_pointer
}

/// Which chain of each family every slot was found on, from one walk of
/// each: the consistency oracle's view of the links.
#[cfg(any(test, feature = "test-support"))]
struct Walked {
    bucket: Vec<Option<RecordKey>>,
    exact: Vec<Option<u64>>,
    flush: Vec<Option<RecordKey>>,
    source: Vec<Option<IpAddr>>,
    /// `Some(true)` on the unwatched recency list, `Some(false)` on the
    /// watched one.
    unwatched: Vec<Option<bool>>,
}

#[cfg(any(test, feature = "test-support"))]
impl Walked {
    fn of(cache: &RecordCache) -> Self {
        let len = cache.slots.len();
        let mut walked = Self {
            bucket: alloc::vec![None; len],
            exact: alloc::vec![None; len],
            flush: alloc::vec![None; len],
            source: alloc::vec![None; len],
            unwatched: alloc::vec![None; len],
        };
        let slots = &cache.slots;
        for (key, chain) in &cache.buckets {
            mark(
                slots,
                chain,
                |slot| slot.bucket_link,
                *key,
                &mut walked.bucket,
            );
        }
        for (exact, chain) in &cache.exact {
            mark(
                slots,
                chain,
                |slot| slot.exact_link,
                *exact,
                &mut walked.exact,
            );
        }
        for (key, flushable) in &cache.flushable {
            let chain = &flushable.chain;
            mark(
                slots,
                chain,
                |slot| slot.flush_link,
                *key,
                &mut walked.flush,
            );
        }
        for (source, chain) in &cache.sources {
            mark(
                slots,
                chain,
                |slot| slot.source_link,
                *source,
                &mut walked.source,
            );
        }
        let recency = |slot: &Slot| slot.recency_link;
        mark(slots, &cache.watched, recency, false, &mut walked.unwatched);
        mark(
            slots,
            &cache.unwatched,
            recency,
            true,
            &mut walked.unwatched,
        );
        walked
    }

    fn holds_nothing_at(&self, index: usize) -> bool {
        self.bucket[index].is_none()
            && self.exact[index].is_none()
            && self.flush[index].is_none()
            && self.source[index].is_none()
            && self.unwatched[index].is_none()
    }
}

/// Walk `list` through `link`, marking each slot it holds with `tag`; a
/// chain that loops, names a slot past the table, miscounts itself, or
/// holds a slot another chain of its family holds is corrupt.
#[cfg(any(test, feature = "test-support"))]
fn mark<T: Copy>(
    slots: &[Option<Slot>],
    list: &IntrusiveList,
    link: fn(&Slot) -> Link,
    tag: T,
    marks: &mut [Option<T>],
) {
    let mut next = list.front();
    let mut seen = 0usize;
    while let Some(at) = next {
        seen += 1;
        assert!(seen <= slots.len(), "a chain that loops");
        let mark = marks
            .get_mut(at)
            .expect("a chain names a slot past the table");
        assert!(mark.is_none(), "a slot on two chains of one family");
        *mark = Some(tag);
        next = slots
            .get(at)
            .and_then(Option::as_ref)
            .and_then(|slot| link(slot).next());
    }
    assert_eq!(seen, list.len(), "a chain miscounts itself");
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
/// once every point has passed (RFC 6762 §5.2), spread by up to
/// [`REFRESH_SPREAD_PERCENT`] of its lifetime by `spread`'s high half.
fn refresh_instant(received: u128, expires: u128, stage: u8, spread: u64) -> u128 {
    let Some(percent) = REFRESH_PERCENTS.get(usize::from(stage)) else {
        return NEVER;
    };
    let lifetime = expires.saturating_sub(received);
    let jitter =
        (lifetime * u128::from(REFRESH_SPREAD_PERCENT) / 100 * u128::from(spread >> 32)) >> 32;
    received.saturating_add(lifetime * u128::from(*percent) / 100 + jitter)
}

#[cfg(test)]
#[path = "mdns_cache_tests.rs"]
mod tests;
