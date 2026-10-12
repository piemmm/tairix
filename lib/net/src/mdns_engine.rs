//! The per-interface multicast DNS responder and querier (RFC 6762 §5–§10).
//!
//! One engine per interface. It owns no socket, no clock, and no randomness:
//! the caller hands it received datagrams with the address and port they came
//! from, monotonic `now` values, a CSPRNG for the jitter the protocol
//! mandates, and a buffer to write outgoing datagrams into.
//!
//! # Tickless
//!
//! [`MdnsEngine::next_deadline`] folds every timer the protocol needs —
//! probe, announce, response delay, query backoff, cache expiry, cache
//! refresh — into one instant. The caller arms a single one-shot for it and
//! calls [`MdnsEngine::poll`] when it fires, repeatedly until it answers
//! `None`. Nothing here spins and nothing here polls.
//!
//! # What a query costs a hostile sender
//!
//! Answering is bounded three ways. A record is multicast at most once per
//! second (RFC 6762 §6), so a flood of identical questions produces one
//! answer. A unicast reply is charged to a per-peer budget and to a budget
//! for the interface, so a peer that asks faster than the segment can
//! usefully carry is answered less, not more. And a query whose source is
//! not on-link is not answered at all — an mDNS reflector is an amplifier
//! with a published multiplier.

use alloc::vec::Vec;

use tairix_abi::time::{Duration64, NANOS_PER_MILLI, NANOS_PER_SEC};
use tairix_collections::{HashMap, IndexedHeap};
use tairix_hash::{BuildFastHash, HashSeed};
use tairix_inline::{ArrayVec, BitSet256, IntrusiveList, Link, LinkStore};

use crate::addr::IpAddr;
use crate::dns::{Name, RecordType};
use crate::rate::PeerBudgets;
use crate::timeutil::{from_nanos, nanos, NEVER};

use super::cache::{expires_at, CachedRecord, KeyedRecord, Learned, RecordCache, RecordKey};
use super::codec::{Message, MessageWriter, Question, Section};
use super::{
    rename, NameKind, QuestionType, RData, Record, TypeBitmap, MAX_MESSAGE_QUESTIONS,
    MAX_PUBLISHED, MAX_QUESTIONS, MAX_RENAMES, PORT, TTL_GOODBYE_SECS,
};

/// One millisecond in the engine's nanosecond time base.
const MS: u128 = NANOS_PER_MILLI as u128;

/// Probes before a unique name is considered claimed (RFC 6762 §8.1).
const PROBE_COUNT: u8 = 3;

/// Interval between probes (RFC 6762 §8.1).
const PROBE_INTERVAL: u128 = 250 * MS;

/// The random delay before the first probe, so hosts powering up together do
/// not probe in lockstep (RFC 6762 §8.1).
const PROBE_FIRST_DELAY_MAX: u128 = 250 * MS;

/// Unsolicited announcements after a successful probe (RFC 6762 §8.3).
const ANNOUNCE_COUNT: u8 = 2;

/// Interval before the second announcement, doubling thereafter (RFC 6762
/// §8.3).
const ANNOUNCE_INTERVAL: u128 = NANOS_PER_SEC as u128;

/// Goodbyes sent when a publication is withdrawn (RFC 6762 §10.1).
const GOODBYE_COUNT: u8 = 2;

/// Interval between goodbyes (RFC 6762 §10.1).
const GOODBYE_INTERVAL: u128 = 250 * MS;

/// How long a host that loses a simultaneous-probe tiebreak waits before
/// probing again (RFC 6762 §8.2).
const TIEBREAK_DEFER: u128 = NANOS_PER_SEC as u128;

/// The multicast response delay window for a shared record (RFC 6762 §6).
const RESPONSE_JITTER_MIN: u128 = 20 * MS;
const RESPONSE_JITTER_SPAN: u128 = 100 * MS;

/// The longer delay a query promising further known answers earns, so the
/// rest of the list arrives before the answer is composed (RFC 6762 §7.2).
const TRUNCATED_QUERY_DELAY_MIN: u128 = 400 * MS;
const TRUNCATED_QUERY_DELAY_SPAN: u128 = 100 * MS;

/// A record is multicast at most this often (RFC 6762 §6).
const PER_RECORD_INTERVAL: u128 = NANOS_PER_SEC as u128;

/// The delay before a new question's first query, which lets several
/// questions raised together travel in one datagram (RFC 6762 §5.2).
const QUERY_FIRST_DELAY_MAX: u128 = 120 * MS;

/// The first retransmission interval of a continuous query, doubling to
/// [`QUERY_MAX_INTERVAL`] (RFC 6762 §5.2).
const QUERY_INITIAL_INTERVAL: u128 = NANOS_PER_SEC as u128;

/// The interval a continuous query settles at: one hour (RFC 6762 §5.2).
const QUERY_MAX_INTERVAL: u128 = 3600 * NANOS_PER_SEC as u128;

/// The TTL ceiling on a legacy unicast reply (RFC 6762 §6.7), which a
/// resolver with no mDNS cache coherency must not hold for long.
const LEGACY_TTL_CAP: u32 = 10;

/// Peers whose unicast-reply budget is tracked at once.
const MAX_TRACKED_PEERS: usize = 16;

/// Unicast replies one peer may draw in a burst, and the rate they refill
/// at. A peer that asks faster is answered less.
const PEER_REPLY_BURST: u32 = 8;
const PEER_REPLY_RATE: u32 = 4;

/// The same, for the interface as a whole, so tracking only
/// [`MAX_TRACKED_PEERS`] cannot be sidestepped by rotating source addresses.
const LINK_REPLY_BURST: u32 = 40;
const LINK_REPLY_RATE: u32 = 20;

/// Lifecycle events the engine surfaces for the caller to log and act on.
const MAX_EVENTS: usize = 8;

/// Records compared before two probing hosts are treated as proposing the
/// same thing. A fixed bound on the work one probe datagram can ask for.
const MAX_TIEBREAK_DEPTH: usize = 8;

/// The peer's proposed records considered in that comparison.
const MAX_TIEBREAK_RECORDS: usize = 16;

/// Known answers in one query that are checked against what we publish.
const MAX_KNOWN_ANSWERS: usize = 32;

/// Identifies one publication for its lifetime, including across renames.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct PublishId(u32);

/// Identifies one continuous question: the caller's name for it, which
/// every edge in its answer set carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct QuestionId(u32);

impl QuestionId {
    /// The question its asker calls `raw`.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Its asker's name for it.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Where a datagram the engine built must be sent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Destination {
    /// The link-local mDNS group, on whichever families the interface has
    /// joined.
    Group,
    /// Straight back to one peer, which a `QU` question or a legacy resolver
    /// asked for.
    Peer {
        /// The peer's address.
        addr: IpAddr,
        /// The peer's source port, which is 5353 for a `QU` reply and
        /// something else for a legacy one.
        port: u16,
    },
}

/// Who sent a received datagram, as the network stack attests it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sender {
    /// The source address.
    pub addr: IpAddr,
    /// The source port: 5353 for a multicast DNS peer, anything else for a
    /// legacy resolver (RFC 6762 §6.7).
    pub port: u16,
    /// Whether the stack found `addr` on the receiving interface's own link.
    ///
    /// Only the stack's address table can know, so the engine takes its
    /// verdict rather than a copy of the prefixes that would go stale. A
    /// sender that is not on-link is never answered and nothing it says is
    /// cached: answering off-link turns a host into a reflector, and mDNS
    /// reflection is a documented amplifier.
    pub on_link: bool,
}

/// A datagram the engine wrote into the caller's buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Emit {
    /// Octets written at the front of the buffer.
    pub len: usize,
    /// Where to send them.
    pub to: Destination,
}

/// Where one publication is in its lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceState {
    /// Claiming the name: probing for a conflicting owner (RFC 6762 §8.1).
    Probing,
    /// The name is claimed and the unsolicited announcements are going out
    /// (RFC 6762 §8.3).
    Announcing,
    /// Established: the records are live and answer queries.
    Live,
    /// A simultaneous probe was lost; probing restarts after the RFC 6762
    /// §8.2 wait.
    Deferred,
    /// Withdrawing: the goodbyes are going out (RFC 6762 §10.1).
    Retiring,
    /// Nothing is published, and nothing will be: the rename budget ran out
    /// against a peer that kept claiming the name.
    Withdrawn,
}

/// Why a publication or a question was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishError {
    /// [`MAX_PUBLISHED`] records, or [`MAX_QUESTIONS`] questions, are
    /// already held on this interface.
    TooMany,
    /// A publication must carry at least one record.
    NoRecords,
    /// The allocator refused the publication.
    NoMemory,
    /// That id, or that name and type, is already being asked. One question
    /// per pair: consumers wanting the same answer share the one that
    /// exists.
    Duplicate,
}

/// How one question's answer set moved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnswerChange {
    /// A record answering the question is now held.
    Added,
    /// A held answer was asserted again, renewing its lifetime.
    Refreshed,
    /// A held answer left the cache: its lifetime ran out, a goodbye or its
    /// owner's cache-flush retired it, the bounds evicted it, or the link
    /// went down.
    Retired,
}

/// One edge in one question's answer set.
///
/// Every record a question has been told was [`AnswerChange::Added`] is
/// later told [`AnswerChange::Retired`] exactly once, unless the question
/// is stopped first — so a consumer's set never outlives the cache's.
#[derive(Clone, Copy, Debug)]
pub struct Answer<'a> {
    /// The question the record answers.
    pub question: QuestionId,
    /// What happened to it.
    pub change: AnswerChange,
    /// The record, who sent it, and the lifetime the cache holds it for.
    pub record: &'a CachedRecord,
}

/// Something the caller must know about: every one is an audit-worthy edge
/// in a name's ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MdnsEvent {
    /// Probing finished without a conflict; the name is this host's.
    Established {
        /// The publication that claimed it.
        id: PublishId,
    },
    /// A peer claimed the name, so the publication moved to one
    /// [`MdnsEngine::name_of`] reports and is probing again. Any record
    /// elsewhere that *pointed* at the old name is the caller's to
    /// re-publish; the engine renames an owner, not a reference.
    Renamed {
        /// The publication that moved.
        id: PublishId,
    },
    /// The rename budget ran out. Nothing is published under this id, and
    /// nothing will be until the caller publishes again under a name it
    /// chooses.
    ConflictBudgetExhausted {
        /// The publication that gave up.
        id: PublishId,
    },
}

/// One record this host publishes.
#[derive(Clone, Debug)]
struct Published {
    record: Record,
    group: PublishId,
    /// Whether the engine derived this record rather than the caller
    /// publishing it. A derived `NSEC` is announced and answered with, but
    /// never *proposed*: a probe claims what its owner asked to claim, and
    /// putting a record a peer's probe cannot carry into the RFC 6762 §8.2
    /// comparison would decide every tiebreak in our favour.
    derived: bool,
    /// When this record was last multicast, for the RFC 6762 §6 one-per-
    /// second rule, or `None` while it has never been sent.
    last_sent: Option<u128>,
}

/// One name this host claims, and the lifecycle of that claim.
#[derive(Clone, Debug)]
struct Group {
    id: PublishId,
    name: Name,
    kind: NameKind,
    /// Whether the name is claimed exclusively, which is what makes probing
    /// necessary and sets the cache-flush bit.
    unique: bool,
    state: ServiceState,
    /// The next probe, announcement, or goodbye.
    next_at: u128,
    /// Probes, announcements, or goodbyes still to send.
    remaining: u8,
    /// The announcement interval, doubling as RFC 6762 §8.3 allows.
    interval: u128,
    renames: u8,
}

/// One continuous question this host asks.
///
/// A record type, never `ANY`: an answer set is kept by name and type, and
/// `ANY` is the query type a probe asks with, not a question with a set.
#[derive(Clone, Debug)]
struct Asked {
    id: QuestionId,
    name: Name,
    record_type: RecordType,
    /// The cache's key for the pair, which the question is chained by.
    key: RecordKey,
    /// The retransmission interval, doubling to [`QUERY_MAX_INTERVAL`].
    interval: u128,
    key_link: Link,
}

/// The question slots through their key links.
struct KeyView<'a>(&'a mut [Option<Asked>]);

impl LinkStore for KeyView<'_> {
    fn link(&self, index: usize) -> Option<&Link> {
        self.0.get(index)?.as_ref().map(|asked| &asked.key_link)
    }

    fn link_mut(&mut self, index: usize) -> Option<&mut Link> {
        self.0
            .get_mut(index)?
            .as_mut()
            .map(|asked| &mut asked.key_link)
    }
}

/// The questions one query carries.
type Due = ArrayVec<usize, MAX_MESSAGE_QUESTIONS>;

/// The known answers a query had no room for, which follow it in messages of
/// their own while the instant it was sent at lasts (RFC 6762 §7.2).
#[derive(Debug)]
struct Continuation {
    at: u128,
    /// The questions whose lists remain, the first resuming at `resume`.
    questions: Due,
    resume: usize,
}

/// Every question this host asks, indexed so an arriving record, a received
/// question, or a refresh finds its question without a scan of them.
#[derive(Debug)]
struct Questions {
    slots: Vec<Option<Asked>>,
    /// Each scheduled question's next query instant. Room for a slot is made
    /// when a question takes it, so scheduling one never fails.
    due: IndexedHeap<u128>,
    free: Vec<u32>,
    by_key: HashMap<RecordKey, IntrusiveList, BuildFastHash>,
    by_id: HashMap<QuestionId, u32, BuildFastHash>,
    /// Questions a match has compared: the work counter the tests hold the
    /// per-record path to.
    #[cfg(test)]
    compared: core::cell::Cell<usize>,
}

impl Questions {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            due: IndexedHeap::new(),
            free: Vec::new(),
            by_key: HashMap::with_hasher(BuildFastHash::new()),
            by_id: HashMap::with_hasher(BuildFastHash::new()),
            #[cfg(test)]
            compared: core::cell::Cell::new(0),
        }
    }

    fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    fn get(&self, slot: usize) -> Option<&Asked> {
        self.slots.get(slot)?.as_ref()
    }

    /// The slot asking `name`/`record_type`, whose key is `key`.
    fn find(&self, key: RecordKey, name: &Name, record_type: RecordType) -> Option<usize> {
        let mut next = self.by_key.get(&key).and_then(IntrusiveList::front);
        while let Some(slot) = next {
            #[cfg(test)]
            self.compared.set(self.compared.get() + 1);
            let asked = self.get(slot)?;
            if asked.record_type == record_type && asked.name == *name {
                return Some(slot);
            }
            next = asked.key_link.next();
        }
        None
    }

    /// The question `keyed` answers, if one asks for it.
    fn watcher(&self, keyed: KeyedRecord<'_>) -> Option<QuestionId> {
        if self.is_empty() {
            return None;
        }
        let record = keyed.record();
        let slot = self.find(keyed.key(), &record.name, record.record_type())?;
        self.get(slot).map(|asked| asked.id)
    }

    /// Take `asked` into a slot, due at no instant yet.
    fn insert(&mut self, asked: Asked) -> Result<(), PublishError> {
        if self.by_id.contains_key(&asked.id)
            || self
                .find(asked.key, &asked.name, asked.record_type)
                .is_some()
        {
            return Err(PublishError::Duplicate);
        }
        if self.by_id.len() >= MAX_QUESTIONS {
            return Err(PublishError::TooMany);
        }
        let reused = self.free.last().copied();
        let slot = reused.map_or(self.slots.len(), |slot| slot as usize);
        let handle = u32::try_from(slot).map_err(|_| PublishError::TooMany)?;
        // Every allocation before any change, so a refusal changes nothing; a
        // new slot also reserves its place on the free stack, so releasing it
        // cannot fail and strand it.
        let room = self.by_id.try_reserve(1).is_ok()
            && self.by_key.try_reserve(1).is_ok()
            && self.due.try_reserve_slot(slot).is_ok()
            && (reused.is_some()
                || (self.slots.try_reserve(1).is_ok()
                    && self.free.try_reserve(self.slots.len() + 1).is_ok()));
        if !room {
            return Err(PublishError::NoMemory);
        }
        if reused.is_some() {
            self.free.pop();
        } else {
            self.slots.push(None);
        }
        let (id, key) = (asked.id, asked.key);
        self.slots[slot] = Some(asked);
        // Room for both was reserved above, so neither insertion can fail.
        if !self.by_key.contains_key(&key) {
            let _ = self.by_key.try_insert(key, IntrusiveList::new());
        }
        if let Some(chain) = self.by_key.get_mut(&key) {
            let _ = chain.push_front(&mut KeyView(&mut self.slots), slot);
        }
        let _ = self.by_id.try_insert(id, handle);
        Ok(())
    }

    /// Take question `id` out, returning it.
    fn remove(&mut self, id: QuestionId) -> Option<Asked> {
        let slot = usize::try_from(self.by_id.remove(&id)?).ok()?;
        let key = self.get(slot)?.key;
        if let Some(chain) = self.by_key.get_mut(&key) {
            let _ = chain.unlink(&mut KeyView(&mut self.slots), slot);
            if chain.is_empty() {
                self.by_key.remove(&key);
            }
        }
        self.due.remove(slot);
        if let (Ok(handle), Ok(())) = (u32::try_from(slot), self.free.try_reserve(1)) {
            self.free.push(handle);
        }
        self.slots[slot].take()
    }

    /// Question `id` is due no later than `at`.
    fn pull_forward(&mut self, id: QuestionId, at: u128) {
        let Some(slot) = self
            .by_id
            .get(&id)
            .and_then(|&slot| usize::try_from(slot).ok())
        else {
            return;
        };
        let at = self.due.get(slot).map_or(at, |due| due.min(at));
        let _ = self.due.try_insert(slot, at);
    }

    /// The round at `slot` is spent at `now`: its next is an interval later,
    /// and the interval doubles.
    fn spend(&mut self, slot: usize, now: u128) {
        if let Some(Some(asked)) = self.slots.get_mut(slot) {
            let _ = self
                .due
                .try_insert(slot, now.saturating_add(asked.interval));
            asked.interval = asked.interval.saturating_mul(2).min(QUERY_MAX_INTERVAL);
        }
    }

    /// The questions due at `now`, as many as one query carries, or `None`
    /// when none is.
    fn take_due(&self, now: u128) -> Option<Due> {
        let mut due = Due::new();
        for (slot, _) in self.due.up_to(now).take(MAX_MESSAGE_QUESTIONS) {
            let _ = due.try_push(slot);
        }
        (!due.is_empty()).then_some(due)
    }

    /// The next instant a question is due, or [`NEVER`].
    fn next_due(&self) -> u128 {
        self.due.peek().map_or(NEVER, |(_, at)| at)
    }
}

/// Tell `answers` that `record` left `question`'s answer set.
fn tell_retired(answers: &mut dyn FnMut(&Answer<'_>), question: QuestionId, record: &CachedRecord) {
    answers(&Answer {
        question,
        change: AnswerChange::Retired,
        record,
    });
}

/// A multicast response being accumulated before it is sent.
///
/// Holds *which* published records answer rather than copies of them: every
/// record a responder sends is one it publishes, so a bitmap over the
/// published set is the whole state, and coalescing two queries that want
/// overlapping answers is a union.
#[derive(Clone, Debug)]
struct Pending {
    due: u128,
    records: BitSet256,
    /// Whether the remainder of a probe defence is among them, which keeps
    /// the defence's exemption from the one-per-second rule.
    waived: bool,
}

impl Pending {
    const fn idle() -> Self {
        Self {
            due: NEVER,
            records: BitSet256::new(),
            waived: false,
        }
    }

    /// Send `records` as soon as the engine is next polled.
    fn hasten(&mut self, records: &BitSet256, now_ns: u128, waived: bool) {
        self.records = self.records.union(records);
        self.due = self.due.min(now_ns);
        self.waived |= waived;
    }

    fn is_idle(&self) -> bool {
        self.due == NEVER
    }
}

/// The shape one response takes, which the four RFC 6762 cases differ in:
/// a delayed multicast answer, an immediate probe defence, a `QU` unicast
/// reply, and a legacy unicast reply.
#[derive(Debug)]
struct ResponsePlan<'a> {
    /// Zero except on a legacy reply, which echoes the query's id.
    id: u16,
    /// The question a legacy reply echoes; no other response carries one.
    echo: Option<&'a Question>,
    /// The TTL ceiling a legacy reply imposes.
    ttl_cap: Option<u32>,
    /// Whether to clear the cache-flush bit, which means nothing to a
    /// resolver with no mDNS cache and would make it discard records it
    /// should keep.
    strip_flush: bool,
    /// Whether the datagram goes to the group, which is what makes the
    /// RFC 6762 §6 one-per-second rule apply.
    multicast: bool,
    /// Whether that rule is waived, which only a probe defence does.
    waive_rate_limit: bool,
}

/// The per-interface multicast DNS engine.
#[derive(Debug)]
pub struct MdnsEngine {
    cache: RecordCache,
    published: Vec<Published>,
    groups: Vec<Group>,
    questions: Questions,
    pending: Pending,
    known: Option<Continuation>,
    replies: PeerBudgets<MAX_TRACKED_PEERS>,
    events: ArrayVec<MdnsEvent, MAX_EVENTS>,
    next_id: u32,
}

impl MdnsEngine {
    /// A new engine for one interface, publishing nothing and asking
    /// nothing, whose cache index is keyed with `hash_key` — the per-boot
    /// secret, since a peer chooses the names cached under it.
    #[must_use]
    pub fn new(hash_key: HashSeed) -> Self {
        Self {
            cache: RecordCache::new(hash_key),
            published: Vec::new(),
            groups: Vec::new(),
            questions: Questions::new(),
            pending: Pending::idle(),
            known: None,
            replies: PeerBudgets::new(
                PEER_REPLY_BURST,
                PEER_REPLY_RATE,
                LINK_REPLY_BURST,
                LINK_REPLY_RATE,
            ),
            events: ArrayVec::new(),
            next_id: 0,
        }
    }

    /// The records learned from the segment.
    #[must_use]
    pub fn cache(&self) -> &RecordCache {
        &self.cache
    }

    /// The questions compared since the last call.
    #[cfg(test)]
    pub(crate) fn take_compared(&self) -> usize {
        self.questions.compared.take()
    }

    /// The next lifecycle event, oldest first.
    pub fn take_event(&mut self) -> Option<MdnsEvent> {
        self.events.remove(0)
    }

    /// Where a publication is in its lifecycle, or `None` for an id this
    /// engine does not hold.
    #[must_use]
    pub fn state_of(&self, id: PublishId) -> Option<ServiceState> {
        self.group(id).map(|group| group.state)
    }

    /// The name a publication currently claims, which a conflict may have
    /// moved since it was published.
    #[must_use]
    pub fn name_of(&self, id: PublishId) -> Option<Name> {
        self.group(id).map(|group| group.name)
    }

    /// Publish `data` at `name`.
    ///
    /// A `unique` publication claims the name exclusively: it is probed for
    /// before it is announced, its records carry the cache-flush bit, and a
    /// conflict renames it under [`NameKind`]'s convention. A shared
    /// publication — a service type's `PTR` to an instance, which every host
    /// offering that type contributes to — is announced without probing,
    /// because no one host owns it.
    ///
    /// A unique publication also gains an `NSEC` record asserting exactly
    /// the types published at the name (RFC 6762 §6.1), so a querier asking
    /// for a type this name does not have is told so instead of retrying.
    ///
    /// # Errors
    ///
    /// [`PublishError::NoRecords`] for an empty `data`,
    /// [`PublishError::TooMany`] past [`MAX_PUBLISHED`], and
    /// [`PublishError::NoMemory`] when the allocator refuses.
    pub fn publish(
        &mut self,
        now: Duration64,
        name: Name,
        kind: NameKind,
        unique: bool,
        data: &[RData],
        rng: &mut dyn FnMut() -> u32,
    ) -> Result<PublishId, PublishError> {
        if data.is_empty() {
            return Err(PublishError::NoRecords);
        }
        let extra = usize::from(unique);
        if self.published.len() + data.len() + extra > MAX_PUBLISHED {
            return Err(PublishError::TooMany);
        }
        self.published
            .try_reserve(data.len() + extra)
            .map_err(|_| PublishError::NoMemory)?;
        self.groups
            .try_reserve(1)
            .map_err(|_| PublishError::NoMemory)?;

        let id = PublishId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        let now_ns = nanos(now);
        for item in data {
            self.published.push(Published {
                record: if unique {
                    Record::unique(name, *item)
                } else {
                    Record::shared(name, *item)
                },
                group: id,
                derived: false,
                last_sent: None,
            });
        }
        if unique {
            let mut bitmap = TypeBitmap::new();
            for item in data {
                bitmap.insert(item.record_type());
            }
            bitmap.insert(RecordType::Nsec);
            self.published.push(Published {
                record: Record::unique(name, RData::Nsec(bitmap)),
                group: id,
                derived: true,
                last_sent: None,
            });
        }

        let (state, delay) = if unique {
            (ServiceState::Probing, jitter(rng, 0, PROBE_FIRST_DELAY_MAX))
        } else {
            (ServiceState::Announcing, 0)
        };
        self.groups.push(Group {
            id,
            name,
            kind,
            unique,
            state,
            next_at: now_ns.saturating_add(delay),
            remaining: if unique { PROBE_COUNT } else { ANNOUNCE_COUNT },
            interval: ANNOUNCE_INTERVAL,
            renames: 0,
        });
        Ok(id)
    }

    /// Withdraw a publication, sending the RFC 6762 §10.1 goodbyes so every
    /// querier drops it now rather than when its TTL runs out.
    pub fn withdraw(&mut self, now: Duration64, id: PublishId) {
        let now_ns = nanos(now);
        let Some(group) = self.groups.iter_mut().find(|group| group.id == id) else {
            return;
        };
        // A name that was never established has nothing to take back.
        if matches!(group.state, ServiceState::Probing | ServiceState::Deferred) {
            group.state = ServiceState::Withdrawn;
            self.forget(id);
            return;
        }
        group.state = ServiceState::Retiring;
        group.remaining = GOODBYE_COUNT;
        group.next_at = now_ns;
    }

    /// Ask a continuous question under the caller's `id`, re-asked on the RFC
    /// 6762 §5.2 backoff and refreshed before a matching cached record
    /// expires. The id may be asked again once the question is stopped.
    ///
    /// The answer set starts with what is already cached: each matching
    /// record is told to `answers` as [`AnswerChange::Added`] before this
    /// returns, and every later edge arrives through [`Self::on_message`],
    /// [`Self::poll`], and [`Self::on_link_down`].
    ///
    /// # Errors
    ///
    /// [`PublishError::Duplicate`] when the id or the pair is already asked,
    /// [`PublishError::TooMany`] past [`MAX_QUESTIONS`], and
    /// [`PublishError::NoMemory`] when the allocator refuses.
    pub fn ask(
        &mut self,
        now: Duration64,
        id: QuestionId,
        name: Name,
        record_type: RecordType,
        rng: &mut dyn FnMut() -> u32,
        answers: &mut dyn FnMut(&Answer<'_>),
    ) -> Result<(), PublishError> {
        let key = self.cache.key_of(&name, record_type);
        self.questions.insert(Asked {
            id,
            name,
            record_type,
            key,
            interval: QUERY_INITIAL_INTERVAL,
            key_link: Link::UNLINKED,
        })?;
        let first = nanos(now).saturating_add(jitter(rng, 0, QUERY_FIRST_DELAY_MAX));
        self.questions.pull_forward(id, first);
        self.cache.set_watched(key, &name, record_type, Some(id));
        for cached in self.cache.lookup(key, &name, record_type) {
            answers(&Answer {
                question: id,
                change: AnswerChange::Added,
                record: &cached,
            });
        }
        Ok(())
    }

    /// Stop asking a question, which also stops refreshing the records it
    /// was keeping alive. Its answer set simply ends: no edge is told for it.
    pub fn stop_asking(&mut self, id: QuestionId) {
        if let Some(asked) = self.questions.remove(id) {
            // One question per pair, so nothing else was watching it.
            self.cache
                .set_watched(asked.key, &asked.name, asked.record_type, None);
        }
    }

    /// The next instant [`Self::poll`] has work to do, or `None`.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration64> {
        let mut earliest = self.pending.due.min(self.questions.next_due());
        if let Some(known) = &self.known {
            earliest = earliest.min(known.at);
        }
        for group in &self.groups {
            if group.remaining > 0 && !matches!(group.state, ServiceState::Withdrawn) {
                earliest = earliest.min(group.next_at);
            }
        }
        if let Some(cache) = self.cache.next_deadline() {
            earliest = earliest.min(nanos(cache));
        }
        (earliest != NEVER).then(|| from_nanos(earliest))
    }

    /// Perform whatever `now` has reached, writing at most one datagram into
    /// `out`, and telling `answers` of every answer that expired.
    ///
    /// Call repeatedly until it answers `None`: one call emits one datagram,
    /// so a caller with one buffer never has two half-built messages. A
    /// datagram `out` has no room for is not retried: its round is spent as
    /// though sent, so every schedule moves on and [`Self::next_deadline`]
    /// never stays in the past.
    pub fn poll(
        &mut self,
        now: Duration64,
        rng: &mut dyn FnMut() -> u32,
        out: &mut [u8],
        answers: &mut dyn FnMut(&Answer<'_>),
    ) -> Option<Emit> {
        let now_ns = nanos(now);
        self.expire_cache(now, answers);

        if let Some(emit) = self.send_known(now_ns, out) {
            return Some(emit);
        }
        if !self.pending.is_idle() && self.pending.due <= now_ns {
            if let Some(emit) = self.send_pending(now_ns, out) {
                return Some(emit);
            }
        }
        if let Some(emit) = self.advance_groups(now_ns, rng, out) {
            return Some(emit);
        }
        self.send_query(now_ns, out)
    }

    /// Fold one received datagram, answering immediately where the protocol
    /// asks for an immediate answer, and telling `answers` of every edge it
    /// moved in a question's answer set.
    ///
    /// A delayed multicast answer is scheduled rather than returned; it goes
    /// out through [`Self::poll`] when its jitter elapses, which is what lets
    /// several queries arriving together produce one datagram.
    pub fn on_message(
        &mut self,
        now: Duration64,
        bytes: &[u8],
        sender: Sender,
        rng: &mut dyn FnMut() -> u32,
        out: &mut [u8],
        answers: &mut dyn FnMut(&Answer<'_>),
    ) -> Option<Emit> {
        // Off-link first, before a single byte is parsed: a sender that is
        // not on this link has no business asking this host anything.
        if !sender.on_link {
            return None;
        }
        let message = Message::parse(bytes)?;
        if message.is_empty() {
            return None;
        }
        if message.response {
            self.on_response(now, &message, sender.addr, answers);
            return None;
        }
        self.on_query(now, &message, sender.addr, sender.port, rng, out)
    }

    /// Drop everything learned on this link, which is what a link going down
    /// means: a record learned there says nothing about anywhere else. Every
    /// answer a question held is told to `answers` as retired; the questions
    /// themselves are the caller's to keep or stop.
    pub fn on_link_down(&mut self, answers: &mut dyn FnMut(&Answer<'_>)) {
        self.cache
            .clear(&mut |question, gone| tell_retired(answers, question, gone));
        self.pending = Pending::idle();
    }

    // -- responses ----------------------------------------------------------

    /// Fold a response: cache what it asserts, notice what it claims of
    /// ours, and drop from the pending answer anything it has just said.
    fn on_response(
        &mut self,
        now: Duration64,
        message: &Message<'_>,
        from: IpAddr,
        answers: &mut dyn FnMut(&Answer<'_>),
    ) {
        for (section, record) in message.records() {
            if section == Section::Authority {
                continue;
            }
            if self.conflicts_with_ours(&record) {
                self.on_conflict(now, &record);
                continue;
            }
            let keyed = self.cache.keyed(&record);
            let watcher = self.questions.watcher(keyed);
            let learned = self
                .cache
                .learn(now, keyed, from, watcher, &mut |question, gone| {
                    tell_retired(answers, question, gone);
                });
            let change = match learned {
                Learned::Ignored => continue,
                // A goodbye's record stays held for its final second, and
                // leaves through the expiry that ends it.
                Learned::Retired => None,
                Learned::Added => Some(AnswerChange::Added),
                Learned::Refreshed => Some(AnswerChange::Refreshed),
            };
            if let (Some(change), Some(question)) = (change, watcher) {
                let held = CachedRecord {
                    record,
                    source: from,
                    received: now,
                    expires: from_nanos(expires_at(nanos(now), record.ttl)),
                };
                answers(&Answer {
                    question,
                    change,
                    record: &held,
                });
            }
            // RFC 6762 §7.4: a record another responder has just sent needs
            // not be sent again by us.
            self.suppress_pending(&record);
        }
    }

    /// Whether `record` claims a unique name of ours with data we did not
    /// publish — the RFC 6762 §9 conflict.
    fn conflicts_with_ours(&self, record: &Record) -> bool {
        let owns = self.groups.iter().any(|group| {
            group.unique
                && group.name == record.name
                && !matches!(group.state, ServiceState::Withdrawn)
        });
        if !owns {
            return false;
        }
        let mine = self
            .published
            .iter()
            .filter(|p| p.record.name == record.name)
            .filter(|p| p.record.record_type() == record.record_type());
        let mut any = false;
        for published in mine {
            any = true;
            if published.record.data == record.data {
                return false;
            }
        }
        // A type we hold at that name, asserted differently, is a conflict; a
        // type we do not hold there is somebody else's business.
        any
    }

    /// React to a peer claiming one of our unique names: rename and probe
    /// again, or give up when the budget is spent.
    fn on_conflict(&mut self, now: Duration64, record: &Record) {
        let now_ns = nanos(now);
        let Some(index) = self.groups.iter().position(|group| {
            group.unique
                && group.name == record.name
                && !matches!(group.state, ServiceState::Withdrawn)
        }) else {
            return;
        };
        let (id, kind, old) = {
            let group = &self.groups[index];
            (group.id, group.kind, group.name)
        };
        if self.groups[index].renames >= MAX_RENAMES {
            self.groups[index].state = ServiceState::Withdrawn;
            self.groups[index].remaining = 0;
            self.forget(id);
            let _ = self
                .events
                .try_push(MdnsEvent::ConflictBudgetExhausted { id });
            return;
        }
        let Ok(next) = rename(&old, kind) else {
            self.groups[index].state = ServiceState::Withdrawn;
            self.groups[index].remaining = 0;
            self.forget(id);
            let _ = self
                .events
                .try_push(MdnsEvent::ConflictBudgetExhausted { id });
            return;
        };
        {
            let group = &mut self.groups[index];
            group.name = next;
            group.renames += 1;
            group.state = ServiceState::Probing;
            group.remaining = PROBE_COUNT;
            group.next_at = now_ns;
            group.interval = ANNOUNCE_INTERVAL;
        }
        for published in self.published.iter_mut().filter(|p| p.group == id) {
            published.record.name = next;
            published.last_sent = None;
        }
        self.drop_pending_of(id);
        let _ = self.events.try_push(MdnsEvent::Renamed { id });
    }

    // -- queries ------------------------------------------------------------

    /// Answer a query: immediately where the protocol says immediately, and
    /// on a delay where it says delay.
    fn on_query(
        &mut self,
        now: Duration64,
        message: &Message<'_>,
        from: IpAddr,
        from_port: u16,
        rng: &mut dyn FnMut() -> u32,
        out: &mut [u8],
    ) -> Option<Emit> {
        let now_ns = nanos(now);
        // A probe carries the records its sender proposes to claim in the
        // authority section, and a probe for a name we are also probing for
        // is settled by the RFC 6762 §8.2 tiebreak rather than by answering.
        let probe = message
            .records()
            .any(|(section, _)| section == Section::Authority);
        if probe {
            self.tiebreak(now_ns, message);
        }
        self.suppress_duplicate_questions(now, message);

        // RFC 6762 §6.7: a query from a port other than 5353 is a legacy
        // resolver speaking plain DNS. Its reply is unicast, echoes the
        // question and the id, and carries a short TTL.
        let legacy = from_port != PORT;

        // Both of these are folded once per datagram, never once per
        // (question, record) pair: a peer chooses the question count and
        // the known-answer count, and multiplying them together would let
        // it choose our cost.
        let answerable = self.answerable();
        let suppressed = self.suppressed_by_known_answers(message);

        let mut immediate = BitSet256::new();
        let mut delayed = BitSet256::new();
        let mut echo: Option<Question> = None;
        // A legacy resolver speaks plain DNS, where a query carries one
        // question and the reply echoes it; answering its other questions
        // under one echoed question would misdescribe the reply.
        for question in message
            .questions()
            .take(if legacy { 1 } else { usize::MAX })
        {
            let unicast = legacy || question.unicast_response;
            for bit in &answerable {
                if suppressed.contains(bit) {
                    continue;
                }
                let Some(published) = self.published.get(usize::from(bit)) else {
                    continue;
                };
                if !question.answered_by(&published.record) {
                    continue;
                }
                if probe && !legacy {
                    // Defending a name we own. Answered at once and
                    // multicast, so every host on the segment sees the
                    // claim — and never charged to a budget, because
                    // whether this host keeps its own name must not depend
                    // on one a flood can drain.
                    immediate.insert(bit);
                } else if unicast {
                    immediate.insert(bit);
                    echo.get_or_insert(question);
                } else {
                    delayed.insert(bit);
                }
            }
        }

        if !delayed.is_empty() {
            let delay = if message.truncated {
                jitter(rng, TRUNCATED_QUERY_DELAY_MIN, TRUNCATED_QUERY_DELAY_SPAN)
            } else {
                jitter(rng, RESPONSE_JITTER_MIN, RESPONSE_JITTER_SPAN)
            };
            self.pending.records = self.pending.records.union(&delayed);
            self.pending.due = self.pending.due.min(now_ns.saturating_add(delay));
        }
        if immediate.is_empty() {
            return None;
        }

        let defending = probe && !legacy;
        let plan = if legacy {
            ResponsePlan {
                id: message.id,
                echo: echo.as_ref(),
                ttl_cap: Some(LEGACY_TTL_CAP),
                strip_flush: true,
                multicast: false,
                waive_rate_limit: false,
            }
        } else if defending {
            ResponsePlan {
                id: 0,
                echo: None,
                ttl_cap: None,
                strip_flush: false,
                multicast: true,
                // RFC 6762 §6 exempts a probe response from the
                // one-per-second rule: a prober that is not answered claims
                // the name.
                waive_rate_limit: true,
            }
        } else {
            ResponsePlan {
                id: 0,
                echo: None,
                ttl_cap: None,
                strip_flush: false,
                multicast: false,
                waive_rate_limit: false,
            }
        };
        if !plan.multicast && !self.replies.allow(now, from) {
            return None;
        }
        let destination = if plan.multicast {
            Destination::Group
        } else {
            Destination::Peer {
                addr: from,
                port: if legacy { from_port } else { PORT },
            }
        };
        let (len, unsent) = self.build_response(now_ns, out, &immediate, &plan)?;
        if plan.multicast && !unsent.is_empty() {
            self.pending.hasten(&unsent, now_ns, plan.waive_rate_limit);
        }
        Some(Emit {
            len,
            to: destination,
        })
    }

    /// The published records a query may currently be answered with: those
    /// whose name this host has finished claiming.
    ///
    /// A name still being probed for is not yet ours to answer with.
    fn answerable(&self) -> BitSet256 {
        let mut set = BitSet256::new();
        for (index, published) in self.published.iter().enumerate() {
            let Ok(bit) = u16::try_from(index) else {
                continue;
            };
            let live = self.groups.iter().any(|group| {
                group.id == published.group
                    && matches!(group.state, ServiceState::Announcing | ServiceState::Live)
            });
            if live {
                set.insert(bit);
            }
        }
        set
    }

    /// The published records the query's own answer section already holds
    /// with more than half their lifetime left (RFC 6762 §7.1).
    ///
    /// At most [`MAX_KNOWN_ANSWERS`] of them are considered. Ignoring the
    /// rest can only cost airtime for a record the peer already had, never
    /// a wrong answer, and it keeps a peer from choosing our cost by
    /// stuffing a query with known answers.
    fn suppressed_by_known_answers(&self, message: &Message<'_>) -> BitSet256 {
        let mut set = BitSet256::new();
        let known = message
            .records()
            .filter(|(section, _)| *section == Section::Answer)
            .take(MAX_KNOWN_ANSWERS);
        for (_, answer) in known {
            for (index, published) in self.published.iter().enumerate() {
                if u64::from(answer.ttl) * 2 <= u64::from(published.record.ttl) {
                    continue;
                }
                if !answer.same_record(&published.record) {
                    continue;
                }
                if let Ok(bit) = u16::try_from(index) {
                    set.insert(bit);
                }
            }
        }
        set
    }

    /// RFC 6762 §7.3: a question another host has just multicast is one we
    /// need not ask, so ours slides to its next scheduled round — unless that
    /// query knows an answer we would not offer ourselves. Responders stay
    /// quiet for what it knows, so counting it as ours would leave us without
    /// those answers, and a peer could keep a question of ours from ever
    /// being answered. A truncated query's list goes on in messages not yet
    /// seen, so it never counts.
    fn suppress_duplicate_questions(&mut self, now: Duration64, message: &Message<'_>) {
        if self.questions.is_empty() || message.truncated {
            return;
        }
        let mut asked = Due::new();
        for question in message.questions() {
            // A QU question's answer goes to its asker alone.
            if question.unicast_response {
                continue;
            }
            let QuestionType::Record(record_type) = question.qtype else {
                continue;
            };
            let key = self.cache.key_of(&question.name, record_type);
            if let Some(slot) = self.questions.find(key, &question.name, record_type) {
                if !asked.contains(&slot) {
                    let _ = asked.try_push(slot);
                }
            }
        }
        if asked.is_empty() {
            return;
        }
        for (section, record) in message.records() {
            if section != Section::Answer {
                continue;
            }
            let keyed = self.cache.keyed(&record);
            let Some(slot) = self
                .questions
                .find(keyed.key(), &record.name, record.record_type())
            else {
                continue;
            };
            if !self.cache.would_offer(now, keyed) {
                asked.retain(|held| *held != slot);
            }
        }
        let now_ns = nanos(now);
        for &slot in &asked {
            self.questions.spend(slot, now_ns);
        }
    }

    /// RFC 6762 §8.2: when two hosts probe for one name at the same
    /// instant, the one whose proposed records sort lower defers.
    ///
    /// Both sets are walked in the RFC's ascending order — class, then
    /// type, then raw rdata — and the first difference decides; a set that
    /// runs out while the other continues is the lower one. Identical sets
    /// are not a conflict at all: two hosts asserting the same records are
    /// not fighting over a name.
    fn tiebreak(&mut self, now_ns: u128, message: &Message<'_>) {
        for index in 0..self.groups.len() {
            let group = &self.groups[index];
            if !matches!(group.state, ServiceState::Probing) {
                continue;
            }
            let (name, id) = (group.name, group.id);
            if self.loses_tiebreak(id, &name, message) {
                let group = &mut self.groups[index];
                group.state = ServiceState::Deferred;
                group.remaining = PROBE_COUNT;
                group.next_at = now_ns.saturating_add(TIEBREAK_DEFER);
            }
        }
    }

    /// Whether our proposed set for `id` sorts below the one `message`
    /// proposes at `name`.
    ///
    /// Walking both sets in order without materialising either keeps the
    /// work a probe can ask for bounded: at most [`MAX_TIEBREAK_DEPTH`]
    /// rounds over at most [`MAX_TIEBREAK_RECORDS`] of the peer's records,
    /// whatever the datagram holds.
    fn loses_tiebreak(&self, id: PublishId, name: &Name, message: &Message<'_>) -> bool {
        let mut ours: Option<TiebreakKey> = None;
        let mut theirs: Option<TiebreakKey> = None;
        for _ in 0..MAX_TIEBREAK_DEPTH {
            ours = self
                .published
                .iter()
                .filter(|p| p.group == id && !p.derived)
                .map(|p| TiebreakKey::of(&p.record))
                .filter(|key| ours.is_none_or(|prev| *key > prev))
                .min();
            theirs = message
                .records()
                .filter(|(section, record)| *section == Section::Authority && record.name == *name)
                .take(MAX_TIEBREAK_RECORDS)
                .map(|(_, record)| TiebreakKey::of(&record))
                .filter(|key| theirs.is_none_or(|prev| *key > prev))
                .min();
            match (ours, theirs) {
                // Our set ran out first while theirs continues: the shorter
                // set is the lower one.
                (None, Some(_)) => return true,
                (None | Some(_), None) => return false,
                (Some(mine), Some(yours)) if mine != yours => return mine < yours,
                (Some(_), Some(_)) => {}
            }
        }
        false
    }

    // -- transmission -------------------------------------------------------

    /// Send the accumulated multicast answer.
    fn send_pending(&mut self, now_ns: u128, out: &mut [u8]) -> Option<Emit> {
        let Pending {
            records: selected,
            waived,
            ..
        } = core::mem::replace(&mut self.pending, Pending::idle());
        if selected.is_empty() {
            return None;
        }
        let plan = ResponsePlan {
            id: 0,
            echo: None,
            ttl_cap: None,
            strip_flush: false,
            multicast: true,
            waive_rate_limit: waived,
        };
        let (len, unsent) = self.build_response(now_ns, out, &selected, &plan)?;
        if !unsent.is_empty() {
            self.pending.hasten(&unsent, now_ns, waived);
        }
        Some(Emit {
            len,
            to: Destination::Group,
        })
    }

    /// Advance the probe, announce, and goodbye schedules, emitting at most
    /// one datagram. A round whose datagram `out` has no room for is spent
    /// all the same, and the walk moves on to the next group.
    fn advance_groups(
        &mut self,
        now_ns: u128,
        rng: &mut dyn FnMut() -> u32,
        out: &mut [u8],
    ) -> Option<Emit> {
        for index in 0..self.groups.len() {
            let group = &self.groups[index];
            if group.next_at > now_ns || group.remaining == 0 {
                continue;
            }
            match group.state {
                ServiceState::Deferred => {
                    self.groups[index].state = ServiceState::Probing;
                    self.groups[index].next_at = now_ns;
                }
                ServiceState::Probing => {
                    let built = self.build_probe(index, out);
                    let group = &mut self.groups[index];
                    group.remaining -= 1;
                    if group.remaining == 0 {
                        group.state = ServiceState::Announcing;
                        group.remaining = ANNOUNCE_COUNT;
                        group.interval = ANNOUNCE_INTERVAL;
                        group.next_at = now_ns;
                        let id = group.id;
                        let _ = self.events.try_push(MdnsEvent::Established { id });
                    } else {
                        group.next_at = now_ns.saturating_add(PROBE_INTERVAL);
                    }
                    let _ = rng;
                    if let Some(len) = built {
                        return Some(Emit {
                            len,
                            to: Destination::Group,
                        });
                    }
                }
                ServiceState::Announcing | ServiceState::Retiring => {
                    let retiring = matches!(group.state, ServiceState::Retiring);
                    let built = self.build_announcement(index, now_ns, out, retiring);
                    let group = &mut self.groups[index];
                    group.remaining -= 1;
                    if group.remaining == 0 {
                        if retiring {
                            group.state = ServiceState::Withdrawn;
                            let id = group.id;
                            self.forget(id);
                        } else {
                            group.state = ServiceState::Live;
                        }
                    } else {
                        let step = if retiring {
                            GOODBYE_INTERVAL
                        } else {
                            group.interval
                        };
                        group.next_at = now_ns.saturating_add(step);
                        group.interval = group.interval.saturating_mul(2);
                    }
                    if let Some(len) = built {
                        return Some(Emit {
                            len,
                            to: Destination::Group,
                        });
                    }
                }
                ServiceState::Live | ServiceState::Withdrawn => {}
            }
        }
        None
    }

    /// Send every question whose schedule `now_ns` has reached, as many as
    /// one message holds, carrying what we already know so responders can
    /// stay quiet (RFC 6762 §7.1).
    ///
    /// Questions raised together travel together (RFC 6762 §5.2), at most
    /// [`MAX_MESSAGE_QUESTIONS`] to a query; a question the message had no
    /// room for stays due and leads the next one. A round `out` cannot hold
    /// even one question of is spent unsent, so a caller that gave no room is
    /// never left with a deadline in the past.
    fn send_query(&mut self, now_ns: u128, out: &mut [u8]) -> Option<Emit> {
        loop {
            let due = self.questions.take_due(now_ns)?;
            let built = self.build_query(now_ns, out, &due);
            let sent = built.map_or(due.len(), |(_, sent)| sent);
            for &slot in &due[..sent] {
                self.questions.spend(slot, now_ns);
            }
            if let Some((len, _)) = built {
                return Some(Emit {
                    len,
                    to: Destination::Group,
                });
            }
            // `out` holds no question at all, so none still due would fit
            // either: their rounds are spent too.
        }
    }

    /// Build one query from the leading `due` questions that fit, followed by
    /// their known answers, returning its length and how many questions it
    /// carries. Known answers it has no room for follow it.
    fn build_query(
        &mut self,
        now_ns: u128,
        out: &mut [u8],
        due: &[usize],
    ) -> Option<(usize, usize)> {
        let mut writer = MessageWriter::new(out, 0, false)?;
        let mut carried = Due::new();
        for &slot in due {
            let asked = self.questions.get(slot)?;
            let question = Question::new(asked.name, QuestionType::Record(asked.record_type));
            if !writer.push_question(&question) {
                break;
            }
            let _ = carried.try_push(slot);
        }
        if carried.is_empty() {
            return None;
        }
        let sent = carried.len();
        self.known = self.push_known(now_ns, &mut writer, &carried, None);
        if self.known.is_some() {
            writer.set_truncated();
        }
        Some((writer.finish(), sent))
    }

    /// The next message of known answers a query had no room for, or `None`
    /// once none remain. The list is dropped if time has moved since the
    /// query: the responders waiting on it have stopped.
    fn send_known(&mut self, now_ns: u128, out: &mut [u8]) -> Option<Emit> {
        let known = self.known.take()?;
        if known.at != now_ns {
            return None;
        }
        let mut writer = MessageWriter::new(out, 0, false)?;
        self.known = self.push_known(now_ns, &mut writer, &known.questions, Some(known.resume));
        if writer.is_empty() {
            return None;
        }
        if self.known.is_some() {
            writer.set_truncated();
        }
        Some(Emit {
            len: writer.finish(),
            to: Destination::Group,
        })
    }

    /// Push the known answers of `questions` into `writer`, the first
    /// question's walk resuming at `resume`, and return what did not fit.
    ///
    /// Each question's records come freshest first, so its list ends at the
    /// first a responder would no longer stay quiet for: anything after it
    /// arrived earlier still. A record that does not fit even an empty
    /// message never will, and is skipped.
    fn push_known(
        &self,
        now_ns: u128,
        writer: &mut MessageWriter<'_>,
        questions: &[usize],
        resume: Option<usize>,
    ) -> Option<Continuation> {
        let now = from_nanos(now_ns);
        for (index, &slot) in questions.iter().enumerate() {
            let asked = self.questions.get(slot)?;
            let mut matches = match resume.filter(|_| index == 0) {
                Some(at) => self.cache.lookup_from(at, &asked.name, asked.record_type),
                None => self.cache.lookup(asked.key, &asked.name, asked.record_type),
            };
            loop {
                let at = matches.position();
                let (Some(at), Some(cached)) = (at, matches.next()) else {
                    break;
                };
                if !cached.worth_offering(now) {
                    break;
                }
                let mut known = cached.record;
                // A known answer carries the lifetime *left*, not the one it
                // arrived with, or a responder would suppress on a record we
                // are about to lose.
                known.ttl = remaining_secs(cached.expires, now_ns);
                known.cache_flush = false;
                if writer.push_record(Section::Answer, &known) || writer.is_empty() {
                    continue;
                }
                let mut rest = Due::new();
                for &left in &questions[index..] {
                    let _ = rest.try_push(left);
                }
                return Some(Continuation {
                    at: now_ns,
                    questions: rest,
                    resume: at,
                });
            }
        }
        None
    }

    /// Build a probe: the name asked about as `ANY`, with the records we
    /// propose to claim in the authority section (RFC 6762 §8.1).
    fn build_probe(&self, index: usize, out: &mut [u8]) -> Option<usize> {
        let group = self.groups.get(index)?;
        let mut writer = MessageWriter::new(out, 0, false)?;
        let mut question = Question::new(group.name, QuestionType::Any);
        // RFC 6762 §8.1 asks the first probes unicast-response, so a
        // conflicting host answers us directly rather than the whole segment.
        question.unicast_response = true;
        if !writer.push_question(&question) {
            return None;
        }
        for published in self
            .published
            .iter()
            .filter(|p| p.group == group.id && !p.derived)
        {
            // The authority section proposes, so it carries no cache-flush
            // bit: nothing is claimed until the probe succeeds.
            let mut proposed = published.record;
            proposed.cache_flush = false;
            if !writer.push_record(Section::Authority, &proposed) {
                break;
            }
        }
        Some(writer.finish())
    }

    /// Build an announcement, or the goodbye that withdraws one.
    fn build_announcement(
        &mut self,
        index: usize,
        now_ns: u128,
        out: &mut [u8],
        goodbye: bool,
    ) -> Option<usize> {
        let id = self.groups.get(index)?.id;
        let mut writer = MessageWriter::new(out, 0, true)?;
        let mut any = false;
        for published in self.published.iter_mut().filter(|p| p.group == id) {
            let mut record = published.record;
            if goodbye {
                record.ttl = TTL_GOODBYE_SECS;
            }
            if !writer.push_record(Section::Answer, &record) {
                break;
            }
            published.last_sent = Some(now_ns);
            any = true;
        }
        any.then(|| writer.finish())
    }

    /// Build a response carrying the selected published records, returning
    /// its length and the selected records it had no room for.
    ///
    /// A unicast reply that runs out of room says so with the truncation bit;
    /// a multicast response never sets it (RFC 6762 §18.5), and what did not
    /// fit is the caller's to send next.
    fn build_response(
        &mut self,
        now_ns: u128,
        out: &mut [u8],
        selected: &BitSet256,
        plan: &ResponsePlan<'_>,
    ) -> Option<(usize, BitSet256)> {
        let mut writer = MessageWriter::new(out, plan.id, true)?;
        if let Some(question) = plan.echo {
            if !writer.push_question(question) {
                return None;
            }
        }
        let mut any = false;
        let mut unsent = BitSet256::new();
        for bit in selected {
            if !unsent.is_empty() {
                unsent.insert(bit);
                continue;
            }
            let index = usize::from(bit);
            let Some(published) = self.published.get(index) else {
                continue;
            };
            let too_soon = published
                .last_sent
                .is_some_and(|sent| sent.saturating_add(PER_RECORD_INTERVAL) > now_ns);
            if plan.multicast && !plan.waive_rate_limit && too_soon {
                continue;
            }
            let mut record = published.record;
            if let Some(cap) = plan.ttl_cap {
                record.ttl = record.ttl.min(cap);
            }
            if plan.strip_flush {
                record.cache_flush = false;
            }
            if !writer.push_record(Section::Answer, &record) {
                if !plan.multicast {
                    writer.set_truncated();
                    break;
                }
                unsent.insert(bit);
                continue;
            }
            if plan.multicast {
                if let Some(published) = self.published.get_mut(index) {
                    published.last_sent = Some(now_ns);
                }
            }
            any = true;
        }
        any.then(|| (writer.finish(), unsent))
    }

    // -- budgets and housekeeping -------------------------------------------

    /// Expire cached records and pull forward the questions whose answers
    /// are about to go stale (RFC 6762 §5.2).
    fn expire_cache(&mut self, now: Duration64, answers: &mut dyn FnMut(&Answer<'_>)) {
        let now_ns = nanos(now);
        let Self {
            cache, questions, ..
        } = self;
        cache.advance(
            now,
            &mut |question| questions.pull_forward(question, now_ns),
            &mut |question, gone| tell_retired(answers, question, gone),
        );
    }

    /// Drop a record from the pending answer because another responder has
    /// just sent it (RFC 6762 §7.4).
    fn suppress_pending(&mut self, record: &Record) {
        if self.pending.is_idle() {
            return;
        }
        let pending = self.pending.records;
        for bit in &pending {
            let Some(published) = self.published.get(usize::from(bit)) else {
                continue;
            };
            if published.record.same_record(record) {
                self.pending.records.remove(bit);
            }
        }
        if self.pending.records.is_empty() {
            self.pending = Pending::idle();
        }
    }

    /// Drop a whole publication's records from the pending answer, which a
    /// rename must do: they no longer carry the name they were selected for.
    fn drop_pending_of(&mut self, id: PublishId) {
        for index in 0..self.published.len() {
            if self.published.get(index).map(|p| p.group) != Some(id) {
                continue;
            }
            if let Ok(bit) = u16::try_from(index) {
                self.pending.records.remove(bit);
            }
        }
        if self.pending.records.is_empty() {
            self.pending = Pending::idle();
        }
    }

    /// Drop a withdrawn publication's records.
    ///
    /// The published set is indexed by position, so removing entries would
    /// invalidate the pending bitmap; it is rebuilt here rather than left to
    /// name a record that has moved.
    fn forget(&mut self, id: PublishId) {
        self.drop_pending_of(id);
        let mut moved = BitSet256::new();
        let mut write = 0usize;
        for read in 0..self.published.len() {
            if self.published[read].group == id {
                continue;
            }
            if let (Ok(from), Ok(to)) = (u16::try_from(read), u16::try_from(write)) {
                if self.pending.records.contains(from) {
                    moved.insert(to);
                }
            }
            self.published.swap(write, read);
            write += 1;
        }
        self.published.truncate(write);
        self.pending.records = moved;
        if self.pending.records.is_empty() {
            self.pending = Pending::idle();
        }
    }

    fn group(&self, id: PublishId) -> Option<&Group> {
        self.groups.iter().find(|group| group.id == id)
    }
}

/// A uniformly distributed delay in `[min, min + span)`.
fn jitter(rng: &mut dyn FnMut() -> u32, min: u128, span: u128) -> u128 {
    if span == 0 {
        return min;
    }
    min.saturating_add(u128::from(rng()) % span)
}

/// Seconds of lifetime left, rounded down, saturating at zero.
fn remaining_secs(expires: Duration64, now_ns: u128) -> u32 {
    let left = nanos(expires).saturating_sub(now_ns);
    u32::try_from(left / u128::from(NANOS_PER_SEC)).unwrap_or(u32::MAX)
}

/// The RFC 6762 §8.2 comparison key of a record: class (which is always
/// `IN` here), then type, then the raw rdata octets.
///
/// The names inside an `SRV` target or a `PTR` are compared case-folded,
/// which is the only reading that makes the order agree between two hosts
/// that spelled one name differently.
#[derive(Clone, Copy, Debug)]
struct TiebreakKey {
    record_type: u16,
    data: Record,
}

impl TiebreakKey {
    fn of(record: &Record) -> Self {
        Self {
            record_type: record.record_type().value(),
            data: *record,
        }
    }
}

impl PartialEq for TiebreakKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == core::cmp::Ordering::Equal
    }
}

impl Eq for TiebreakKey {}

impl PartialOrd for TiebreakKey {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TiebreakKey {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.record_type
            .cmp(&other.record_type)
            .then_with(|| rdata_order(&self.data.data, &other.data.data))
    }
}

/// Order two rdata values as RFC 6762 §8.2 orders their octets.
fn rdata_order(a: &RData, b: &RData) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    match (a, b) {
        (RData::A(x), RData::A(y)) => x.octets().cmp(&y.octets()),
        (RData::Aaaa(x), RData::Aaaa(y)) => x.octets().cmp(&y.octets()),
        (RData::Ptr(x), RData::Ptr(y)) => x.cmp(y),
        (RData::Srv(x), RData::Srv(y)) => x
            .priority
            .cmp(&y.priority)
            .then(x.weight.cmp(&y.weight))
            .then(x.port.cmp(&y.port))
            .then_with(|| x.target.cmp(&y.target)),
        (RData::Txt(x), RData::Txt(y)) => x.as_octets().cmp(y.as_octets()),
        (RData::Nsec(x), RData::Nsec(y)) => {
            let (mut xs, mut ys) = (x.bits(), y.bits());
            loop {
                match (xs.next(), ys.next()) {
                    (None, None) => return Ordering::Equal,
                    (None, Some(_)) => return Ordering::Less,
                    (Some(_), None) => return Ordering::Greater,
                    (Some(a), Some(b)) if a != b => return a.cmp(&b),
                    _ => {}
                }
            }
        }
        // Different types never reach here: the key compares types first.
        _ => Ordering::Equal,
    }
}

#[cfg(test)]
#[path = "mdns_engine_tests.rs"]
mod tests;
