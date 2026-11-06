//! The advisory byte-range file-lock manager: the kernel side of the
//! `fs_lock` / `fs_lock_query` syscalls (`plans/FILELOCK.md`).
//!
//! A lock is owned by an **open file description** ([`OwnerId`], minted per
//! `fs_open` and shared by every descriptor duplicated or spawn-inherited
//! from it), keyed on the pinned node ([`LockedNode`]) rather than on a path
//! or a reported id, so a rename moves nothing and a write that changes the
//! id a format reports (FAT's) keeps the key two participants agree on. What
//! the manager owns is the conflict decision, the wait-for graph, and the
//! record bookkeeping; parking and waking are the handler's.
//!
//! # Indexes, and why these
//!
//! Per file the authoritative state is one non-overlapping ordered record
//! set *per owner*, which is what release-on-close and the replace/split
//! algebra walk. Conflict detection reads two projections of it:
//!
//! * `exclusive`, every exclusive record on the file in one ordered map.
//!   Exclusive records cannot overlap each other — that is what exclusivity
//!   means — so this map is non-overlapping and an interval query over it is
//!   an exact `O(log n)`: the predecessor of the range plus the records
//!   starting inside it are the whole candidate set.
//! * `shared`, a count. A shared request conflicts only with an exclusive
//!   holder, so it never reads further than the map above; an exclusive
//!   request against a file with no shared records stops there too, which
//!   is the whole cost for the record-per-row workload where every lock is
//!   exclusive.
//!
//! Both change with each record an operation adds or removes, in the same
//! step, so an operation costs what it touches rather than what the file
//! holds; the randomised runs rebuild them from the records after every step
//! and compare.
//!
//! Only an exclusive request on a file that *does* hold shared records walks
//! owners, and it stops at the first conflict. That walk is over
//! participants — not over records — and the record count is bounded per
//! process by the `file-locks` resource limit, so it cannot grow without
//! someone being charged for it.
//!
//! # Memory
//!
//! Every allocation an operation makes is taken before it changes anything,
//! so an acquisition, a queueing, or an unlock that cuts a record is refused
//! `OutOfMemory` with the registry as it was. An unlock of whole records and
//! the release of a description need none, and a release that cannot list
//! whom it frees wakes every waiter on the file — or, with no room even for
//! that, every lock waiter — each to re-test its own request.
//!
//! # Fairness
//!
//! A request conflicts with the current holders and, when it is prepared to
//! wait, with any *earlier-queued* waiter whose range it overlaps
//! incompatibly. That ordering is what stops a stream of readers starving a
//! writer: once the writer queues, an arriving reader that overlaps it
//! queues behind rather than joining the holders ahead of it. Two
//! deliberate exemptions:
//!
//! * A non-blocking request tests only the holders. Its contract is "tell me
//!   whether the lock is free", and it cannot starve anyone by waiting.
//! * A request over a range this owner **already holds** — an upgrade or a
//!   downgrade — tests only the holders. Making it queue behind a newcomer
//!   could only deadlock it: it cannot release what the newcomer waits for
//!   without giving up the range it is converting.
//!
//! Because queue order strictly decreases along a queue edge, queue edges
//! alone cannot form a cycle; a deadlock always involves a holder.
//!
//! # Deadlock
//!
//! A blocking request whose grant would close a cycle is refused with
//! [`Errno::Deadlock`] rather than joining a wait none of the participants
//! could leave, whether a holder is in its way or only an earlier waiter,
//! which may itself be waiting on what the requester holds. The search is an
//! iterative breadth-first walk of the wait-for graph, bounded by the owners it has
//! already visited, so it neither recurses on a depth a hostile process
//! chooses nor expands an owner twice. A waiter's edges are the conflicts
//! acquisition itself decides: the holders in its way and the waiters queued
//! ahead of it, never one behind, and holders alone for a conversion.
//!
//! Owners are descriptions, so the graph sees a cycle whenever each
//! participant blocks and holds through the same description — which is what
//! a program that locks the file it has open does, and covers every
//! self-deadlock. A cycle whose participants block through one description
//! while holding through a *different* one is not detected; that wait ends
//! when a participant closes or exits, and it stays interruptible
//! throughout, so it is recoverable rather than an unkillable hang. Linux
//! detects none of these cases for its equivalent (`F_OFD_SETLKW`) locks.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::{Errno, LockConflict, LockMode, LockRange};
use tairix_collections::{OrderedMap, OrderedSet, TryReserveError};
use tairix_inline::ArrayVec;
use tairix_kernel_sched_api::TaskId;
use tairix_kernel_sec::ProcessId;
use tairix_sync::SpinLock;

use crate::waitq::{self, filed, WakeKey, FILE_LOCK_WAITQ};

/// The node a lock is taken on: the volume registration that pinned it and
/// the id that pin answered, which stay the node's for as long as any
/// descriptor holds it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct LockedNode {
    /// The registration's instance number.
    pub instance: u64,
    /// The pinned id's raw value.
    pub pinned: u64,
}

impl LockedNode {
    /// The key of the node `node` holds pinned.
    #[must_use]
    pub fn of(node: &crate::fs::NodeRef) -> Self {
        Self {
            instance: node.instance(),
            pinned: node.pinned().raw(),
        }
    }
}

/// Identity of one open file description for locking purposes.
///
/// Minted per `fs_open` and carried by the description, so descriptors
/// duplicated or spawn-inherited from it share one owner while a second open
/// of the same file is a different one.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct OwnerId(u64);

/// The two modes a *held* record can be in. [`LockMode::Unlock`] asks for a
/// release, so it can never be one and is not representable here.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Held {
    /// Coexists with other owners' shared records.
    Shared,
    /// Excludes every other owner.
    Exclusive,
}

impl Held {
    /// The requested mode as a held one, or `None` for
    /// [`LockMode::Unlock`].
    #[must_use]
    pub const fn from_mode(mode: LockMode) -> Option<Self> {
        match mode {
            LockMode::Shared => Some(Self::Shared),
            LockMode::Exclusive => Some(Self::Exclusive),
            LockMode::Unlock => None,
        }
    }

    /// The ABI mode this record reports as.
    #[must_use]
    pub const fn as_mode(self) -> LockMode {
        match self {
            Self::Shared => LockMode::Shared,
            Self::Exclusive => LockMode::Exclusive,
        }
    }

    /// Whether two owners may both hold these modes over one byte, decided
    /// by the ABI's single conflict rule.
    #[must_use]
    const fn compatible_with(self, other: Self) -> bool {
        self.as_mode().compatible_with(other.as_mode())
    }
}

/// One owner's record: the range it covers and the mode it holds.
///
/// Keyed by `range.start()` in its owner's map. An owner's records never
/// overlap and same-mode records that abut are merged, so the set is the
/// minimal description of what that owner holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Record {
    range: LockRange,
    held: Held,
}

/// Everything one owner holds on one file, plus the process its records are
/// charged to.
struct OwnerLocks {
    /// The process whose `file-locks` count these records consume, fixed
    /// when the owner first locked this file. A later split keeps the charge
    /// here rather than migrating it to whoever split the record.
    pid: ProcessId,
    records: OrderedMap<u64, Record>,
}

/// The conflict projections of one file's records, kept in step with them.
struct Projection {
    /// Every exclusive record on the file, non-overlapping and ordered by
    /// start — the exact interval index exclusivity makes possible.
    exclusive: OrderedMap<u64, (LockRange, OwnerId)>,
    /// How many shared records the file holds, so an exclusive request can
    /// skip the owner walk when there are none.
    shared: usize,
}

impl Projection {
    /// Take `record` out. Never allocates.
    fn forget(&mut self, record: Record) {
        match record.held {
            Held::Shared => self.shared = self.shared.saturating_sub(1),
            Held::Exclusive => {
                self.exclusive.remove(&record.range.start());
            }
        }
    }

    /// Put `owner`'s `record` in, in room taken before the change began.
    fn note(&mut self, owner: OwnerId, record: Record) {
        match record.held {
            Held::Shared => self.shared += 1,
            Held::Exclusive => {
                let start = record.range.start();
                filed(self.exclusive.try_insert(start, (record.range, owner)));
            }
        }
    }
}

/// One file's lock state, alive only while something holds or awaits a lock
/// on it.
struct FileEntry {
    /// The wait key every waiter on this file registers under. Drawn from a
    /// monotonic counter and never reused, so a stale registration can never
    /// alias a later file's waiters.
    key: u64,
    owners: OrderedMap<OwnerId, OwnerLocks>,
    projection: Projection,
}

impl FileEntry {
    fn new(key: u64) -> Self {
        Self {
            key,
            owners: OrderedMap::new(),
            projection: Projection {
                exclusive: OrderedMap::new(),
                shared: 0,
            },
        }
    }

    /// Whether `owner` already holds any byte of `range`, which makes the
    /// request a conversion rather than an arrival.
    fn holds_any(&self, owner: OwnerId, range: LockRange) -> bool {
        self.owners
            .get(&owner)
            .is_some_and(|locks| overlapping(&locks.records, range).next().is_some())
    }

    /// The first holder in the way of `held` over `range`, ignoring
    /// `owner`'s own records.
    fn holder_conflict(&self, owner: OwnerId, range: LockRange, held: Held) -> Option<Conflict> {
        if let Some(found) = self.exclusive_conflict(owner, range) {
            return Some(found);
        }
        // A shared request is compatible with every shared record, so the
        // exclusive projection above was the whole question.
        if matches!(held, Held::Shared) || self.projection.shared == 0 {
            return None;
        }
        self.owners
            .iter()
            .filter(|(&other, _)| other != owner)
            .find_map(|(&other, locks)| {
                overlapping(&locks.records, range)
                    .find(|record| !held.compatible_with(record.held))
                    .map(|record| Conflict {
                        owner: other,
                        held: record.held,
                        range: record.range,
                        pid: locks.pid,
                    })
            })
    }

    /// The first exclusive record overlapping `range` that `owner` does not
    /// hold.
    fn exclusive_conflict(&self, owner: OwnerId, range: LockRange) -> Option<Conflict> {
        let exclusive = &self.projection.exclusive;
        let predecessor = exclusive
            .range(..range.start())
            .next_back()
            .filter(|(_, (held, _))| held.end_inclusive() >= range.start());
        predecessor
            .into_iter()
            .chain(exclusive.range(range.start()..=range.end_inclusive()))
            .find(|(_, (_, holder))| *holder != owner)
            .map(|(_, &(held_range, holder))| Conflict {
                owner: holder,
                held: Held::Exclusive,
                range: held_range,
                pid: self
                    .owners
                    .get(&holder)
                    .map_or(ProcessId(0), |locks| locks.pid),
            })
    }

    /// Take the room `plan` needs here for `owner` — its records' and the
    /// exclusive projection's — answering, for an owner new to the file, the
    /// container its first records go in.
    fn make_room(
        &mut self,
        owner: OwnerId,
        pid: ProcessId,
        plan: &Plan,
    ) -> Result<Option<OwnerLocks>, TryReserveError> {
        let exclusive = plan.exclusive_inserts();
        self.projection.exclusive.try_reserve(exclusive)?;
        if let Some(locks) = self.owners.get_mut(&owner) {
            locks.records.try_reserve(plan.insert.len())?;
            return Ok(None);
        }
        self.owners.try_reserve(1)?;
        Ok(Some(OwnerLocks {
            pid,
            records: OrderedMap::try_with_capacity(plan.insert.len())?,
        }))
    }

    /// Make `range` be what `plan` says for `owner`, keeping the projections
    /// in step record by record, in the room [`Self::make_room`] took.
    fn apply(&mut self, owner: OwnerId, plan: &Plan) {
        let Some(locks) = self.owners.get_mut(&owner) else {
            return;
        };
        // The records the plan absorbs or cuts back are the ones it was
        // built from, found again rather than listed.
        while let Some(record) = first_taken(&locks.records, plan) {
            locks.records.remove(&record.range.start());
            self.projection.forget(record);
        }
        for &record in &plan.insert {
            filed(locks.records.try_insert(record.range.start(), record));
            self.projection.note(owner, record);
        }
        if locks.records.is_empty() {
            self.owners.remove(&owner);
        }
    }

    /// Whether nothing is held here any more.
    fn is_vacant(&self) -> bool {
        self.owners.is_empty()
    }
}

/// One request waiting in `fs_lock`, or one asking to.
struct Waiter {
    file: LockedNode,
    owner: OwnerId,
    range: LockRange,
    held: Held,
    /// Arrival order across the whole registry. A waiter is blocked by
    /// earlier-queued waiters, never by later ones, so the queue relation is
    /// a strict order that cannot cycle on its own.
    seq: u64,
}

/// One acquisition request: who is asking, for what, and on what terms.
///
/// Named rather than positional because the eight values are easy to
/// transpose — a `pid` and a `task` are both integers, and a `limit` and a
/// range bound both read as numbers — and a transposition here would charge
/// the wrong process or bound the wrong range.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Request {
    /// The node the lock is on.
    pub file: LockedNode,
    /// The open file description asking.
    pub owner: OwnerId,
    /// The process to charge the records to, if this owner holds none yet.
    pub pid: ProcessId,
    /// The calling task, which is what parks and what the queue orders.
    pub task: TaskId,
    /// The range asked for.
    pub range: LockRange,
    /// The mode asked for.
    pub held: Held,
    /// The requester's `file-locks` bound.
    pub limit: u64,
    /// Whether the caller is prepared to park, which brings the queue
    /// ordering and the deadlock search into play.
    pub waiting: bool,
}

/// What one lock the caller cannot have looks like.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    /// The owner holding it.
    pub owner: OwnerId,
    /// The mode held.
    pub held: Held,
    /// The range held.
    pub range: LockRange,
    /// The process the record is charged to and reported for.
    pub pid: ProcessId,
}

impl Conflict {
    /// The record as the query syscall reports it.
    #[must_use]
    pub fn to_abi(self) -> LockConflict {
        LockConflict {
            mode: self.held.as_mode(),
            start: self.range.start(),
            len: self.range.wire_len(),
            pid: self.pid.0,
        }
    }
}

/// Why an acquisition could not be granted now.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// A holder is in the way; waiting could still succeed.
    Held(Conflict),
    /// An earlier-queued waiter is ahead; waiting could still succeed.
    Queued,
    /// Granting would close a cycle of waiters.
    Deadlock,
    /// The request would push the charged process past its `file-locks`
    /// bound.
    LimitExceeded,
    /// The kernel could not hold what the request needs; nothing was
    /// recorded.
    OutOfMemory,
}

impl Refusal {
    /// The stable errno the syscall reports.
    ///
    /// The one mapping, so the acquire path and the release path cannot
    /// disagree about what a refusal means to a caller. A holder in the way
    /// and an earlier waiter ahead are both "you would have had to wait":
    /// which of the two it was is the manager's business, not the caller's.
    #[must_use]
    pub const fn to_errno(self) -> Errno {
        match self {
            Self::Held(_) | Self::Queued => Errno::WouldBlock,
            Self::Deadlock => Errno::Deadlock,
            Self::LimitExceeded => Errno::LimitExceeded,
            Self::OutOfMemory => Errno::OutOfMemory,
        }
    }
}

impl From<TryReserveError> for Refusal {
    fn from(_: TryReserveError) -> Self {
        Self::OutOfMemory
    }
}

impl From<alloc::collections::TryReserveError> for Refusal {
    fn from(_: alloc::collections::TryReserveError) -> Self {
        Self::OutOfMemory
    }
}

/// The waiters a state change made able to progress.
#[derive(Debug, Default, Eq, PartialEq)]
pub enum Wakes {
    /// Nobody.
    #[default]
    Nobody,
    /// Exactly these waiters, on the file whose wait key is `key`.
    Named {
        /// The file's wait key.
        key: u64,
        /// The waiters whose blocked ranges the change freed.
        tasks: Vec<TaskId>,
    },
    /// Every waiter on the file whose wait key this is: the change could not
    /// list whom it freed, so each re-tests its own request.
    File(u64),
}

impl Wakes {
    /// Whether anything is to be woken.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Nobody)
    }

    /// Unpark the waiters, with the registry's lock already released.
    pub fn wake(&self) {
        match self {
            Self::Nobody => {}
            Self::Named { key, tasks } => waitq::file_lock_wake(WakeKey::new(*key), tasks),
            Self::File(key) => {
                if let Some(arch) = waitq::wait_arch() {
                    FILE_LOCK_WAITQ.wake_key(arch, WakeKey::new(*key));
                }
            }
        }
    }
}

/// Registry state behind [`REGISTRY`].
///
/// Every map is ordered. A pinned id is its driver's, which a foreign
/// volume's layout chooses, and which owners, processes and tasks stay in the
/// rest is up to what users keep locking: no lookup's cost depends on how the
/// keys fall, and the deadlock search's walk of the waiters costs the rows
/// still queued.
struct Registry {
    files: OrderedMap<LockedNode, FileEntry>,
    /// Which files each owner holds records on, so releasing a description
    /// does not scan every locked file.
    owned: OrderedMap<OwnerId, OrderedSet<LockedNode>>,
    /// Live record count per charged process — the `file-locks` usage.
    charged: OrderedMap<ProcessId, u64>,
    waiters: OrderedMap<TaskId, Waiter>,
    /// The tasks queued per file, so a release considers that file's waiters
    /// rather than every waiter in the system.
    queued: OrderedMap<LockedNode, OrderedSet<TaskId>>,
    next_key: u64,
    next_seq: u64,
}

/// The lock-guarded registry: pure data behind a spin lock, never a mutable
/// static.
static REGISTRY: SpinLock<Registry> = SpinLock::new(Registry {
    files: OrderedMap::new(),
    owned: OrderedMap::new(),
    charged: OrderedMap::new(),
    waiters: OrderedMap::new(),
    queued: OrderedMap::new(),
    next_key: 1,
    next_seq: 1,
});

/// Source of [`OwnerId`]s. Monotonic and never reused, so a reclaimed
/// description's id cannot alias a later one's locks.
static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

/// How many lock records exist system-wide, so the descriptor-release path
/// pays one relaxed load rather than the registry lock while nothing is
/// locked.
static LIVE_RECORDS: AtomicU64 = AtomicU64::new(0);

/// Serialise a test that touches the process-global lock registry, leaving it
/// empty for the caller.
///
/// The registry, the per-process charge map and the system-wide live count are
/// one machine's worth of state, and the reset this performs clears all of it
/// — so a test still holding a lock while a sibling reset ran would find it
/// gone, and grant a conflicting request. This module's own tests and the
/// `fs_lock` syscall-handler tests all hold this one lock, which is why it
/// lives here rather than beside either of them.
#[cfg(test)]
pub(crate) fn registry_guard() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_for_test();
    guard
}

/// Mint the owner identity for a fresh open file description.
#[must_use]
pub fn mint_owner() -> OwnerId {
    OwnerId(NEXT_OWNER.fetch_add(1, Ordering::Relaxed))
}

/// Whether any advisory lock is currently held system-wide.
#[must_use]
pub fn locks_present() -> bool {
    LIVE_RECORDS.load(Ordering::Relaxed) != 0
}

/// The live record count charged to `pid` — its `file-locks` usage.
#[must_use]
pub fn usage(pid: ProcessId) -> u64 {
    REGISTRY.lock().charged.get(&pid).copied().unwrap_or(0)
}

/// Report the first lock standing in the way of `held` over `range` on
/// `file` for `owner`, or `None` when the request would be granted.
///
/// Holders only: a queued waiter holds nothing, so reporting one would name
/// a lock that does not exist.
#[must_use]
pub fn query(file: LockedNode, owner: OwnerId, range: LockRange, held: Held) -> Option<Conflict> {
    let reg = REGISTRY.lock();
    reg.files
        .get(&file)
        .and_then(|entry| entry.holder_conflict(owner, range, held))
}

/// Take the lock `request` asks for.
///
/// A request that is prepared to wait ([`Request::waiting`]) is measured
/// against the queue ordering and the deadlock search as well as the
/// holders; a non-blocking one tests only the holders.
///
/// On success the range is held and every earlier record of this owner
/// inside it is replaced, so an upgrade, a downgrade, and a re-lock are one
/// operation. Returns the waiters the change freed — a downgrade from
/// exclusive to shared releases anyone queued on those bytes.
///
/// # Errors
///
/// The [`Refusal`] saying why: a holder in the way, an earlier waiter ahead,
/// a cycle the grant would close, the record bound, or memory.
pub fn acquire(request: &Request) -> Result<Wakes, Refusal> {
    let &Request {
        file,
        owner,
        pid,
        task,
        range,
        held,
        limit,
        waiting,
    } = request;
    let mut reg = REGISTRY.lock();
    let seq = reg.waiters.get(&task).map_or(u64::MAX, |w| w.seq);
    let converting = reg
        .files
        .get(&file)
        .is_some_and(|entry| entry.holds_any(owner, range));

    let refusal = match reg
        .files
        .get(&file)
        .and_then(|entry| entry.holder_conflict(owner, range, held))
    {
        Some(conflict) => Some(Refusal::Held(conflict)),
        None if waiting && !converting && reg.queue_blocked(file, owner, range, held, seq) => {
            Some(Refusal::Queued)
        }
        None => None,
    };
    if let Some(refusal) = refusal {
        // A waiter queued ahead can itself be waiting on what this owner
        // holds, so a queued refusal is searched exactly as a held one.
        let wanting = Waiter {
            file,
            owner,
            range,
            held,
            seq,
        };
        if waiting && reg.would_deadlock(&wanting)? {
            return Err(Refusal::Deadlock);
        }
        return Err(refusal);
    }

    let locks = reg
        .files
        .get(&file)
        .and_then(|entry| entry.owners.get(&owner));
    let plan = locks.map_or_else(
        || Plan::fresh(range, held),
        |locks| Plan::build(&locks.records, range, Some(held)),
    );
    let charge_pid = locks.map_or(pid, |locks| locks.pid);
    reg.price(charge_pid, plan.delta(), limit)?;
    let fresh = reg.make_room(file, owner, charge_pid, &plan)?;
    let wakes = reg.freed_waiters(file, |queued| queued.overlaps(range));
    reg.commit(file, owner, charge_pid, &plan, fresh);
    Ok(wakes)
}

/// Release whatever `owner` holds over `range` on `file`, splitting a record
/// that straddles the range's edges.
///
/// Bounded by `limit` for the same reason an acquisition is: unlocking the
/// middle of a held range turns one record into two, so a loop could
/// manufacture records without ever locking a new byte. Releasing the whole
/// of a held range never grows the count, so a caller at the bound always
/// has a way out.
///
/// # Errors
///
/// [`Refusal::LimitExceeded`] when the split would pass the record bound, and
/// [`Refusal::OutOfMemory`] when the records it cuts cannot be held. An
/// unlock of whole records is refused neither.
pub fn release(
    file: LockedNode,
    owner: OwnerId,
    range: LockRange,
    limit: u64,
) -> Result<Wakes, Refusal> {
    let mut reg = REGISTRY.lock();
    let Some((charge_pid, plan)) = reg.files.get(&file).and_then(|entry| {
        entry
            .owners
            .get(&owner)
            .map(|locks| (locks.pid, Plan::build(&locks.records, range, None)))
    }) else {
        return Ok(Wakes::Nobody);
    };
    reg.price(charge_pid, plan.delta(), limit)?;
    let fresh = reg.make_room(file, owner, charge_pid, &plan)?;
    let wakes = reg.freed_waiters(file, |queued| queued.overlaps(range));
    reg.commit(file, owner, charge_pid, &plan, fresh);
    reg.disown_if_released(file, owner);
    reg.drop_if_vacant(file);
    Ok(wakes)
}

/// Release everything `owner` holds, on every file — the last descriptor on
/// a description closing, which a process exit does for all of them.
///
/// Never refuses and never needs memory: when even the list of whom it frees
/// cannot be had, every lock waiter is woken here instead, each to re-test
/// its own request, and nothing is returned.
#[must_use]
pub fn release_owner(owner: OwnerId) -> Vec<Wakes> {
    if !locks_present() {
        return Vec::new();
    }
    let released = REGISTRY.lock().release_all(owner);
    released.unwrap_or_else(|| {
        wake_every_waiter();
        Vec::new()
    })
}

/// Wake every task parked in `fs_lock`, each to re-test its own request: what
/// a release that may not fail does when it cannot list whom it freed.
fn wake_every_waiter() {
    if let Some(arch) = waitq::wait_arch() {
        FILE_LOCK_WAITQ.wake_all(arch);
    }
}

/// Join the queue for `held` over `range` on `file`, returning the wait key
/// to park under.
///
/// Registered before the caller parks, so a release landing in the window
/// between the failed attempt and the park still finds the waiter and wakes
/// it rather than leaving it parked on a lock that is already free.
///
/// # Errors
///
/// [`Refusal::OutOfMemory`] when the queue cannot hold the waiter: nothing is
/// queued, and the caller fails rather than parks.
pub fn enqueue(
    file: LockedNode,
    owner: OwnerId,
    task: TaskId,
    range: LockRange,
    held: Held,
) -> Result<u64, Refusal> {
    REGISTRY
        .lock()
        .join_queue(file, owner, task, range, held)
        .map_err(Refusal::from)
}

/// Leave the queue once the wait was granted its lock. Idempotent.
pub fn dequeue(task: TaskId) {
    let mut reg = REGISTRY.lock();
    if let Some(waiter) = reg.unqueue(task) {
        reg.drop_if_vacant(waiter.file);
    }
}

/// Leave the queue without the lock — the wait timed out, was signalled, or
/// had no room to register — answering the waiters on ranges it overlaps: its
/// place in the queue may have been all that held them back. Idempotent.
#[must_use]
pub fn withdraw(task: TaskId) -> Wakes {
    let mut reg = REGISTRY.lock();
    let Some(waiter) = reg.unqueue(task) else {
        return Wakes::Nobody;
    };
    let wakes = reg.freed_waiters(waiter.file, |queued| queued.overlaps(waiter.range));
    reg.drop_if_vacant(waiter.file);
    wakes
}

/// Records one replace puts in at most: the parts of a straddling record of
/// the other mode either side of the range, and the new record.
const PLAN_INSERTS: usize = 3;

/// The record edits one replace performs: how many records leave, and which
/// arrive. Computed before anything is touched so the caller can price it
/// against the resource limit first, and allocation-free.
struct Plan {
    /// The range being replaced.
    range: LockRange,
    /// What it becomes: a mode, or `None` for a release.
    new: Option<Held>,
    /// How many records the replace takes out.
    removes: usize,
    /// The records it puts in.
    insert: ArrayVec<Record, PLAN_INSERTS>,
}

impl Plan {
    /// A replace of `range` with `new` that changes nothing yet.
    fn empty(range: LockRange, new: Option<Held>) -> Self {
        Self {
            range,
            new,
            removes: 0,
            insert: ArrayVec::new(),
        }
    }

    /// The edits for an owner that holds nothing on this file yet.
    fn fresh(range: LockRange, held: Held) -> Self {
        let mut plan = Self::empty(range, Some(held));
        plan.put(Record { range, held });
        plan
    }

    /// The edits that make `range` be `new` in `records`.
    ///
    /// A record of the *same* mode that overlaps or merely touches the range
    /// is absorbed into one covering record, so extending a lock byte by
    /// byte does not accumulate a record per byte. A record of the *other*
    /// mode is cut back to the parts outside the range, which is how a
    /// partial unlock and a partial upgrade split one record into two.
    fn build(records: &OrderedMap<u64, Record>, range: LockRange, new: Option<Held>) -> Self {
        let mut plan = Self::empty(range, new);
        let mut span = range;
        for record in touching(records, range) {
            if !plan.takes(record) {
                // Abutting, different mode: untouched.
                continue;
            }
            plan.removes += 1;
            if new == Some(record.held) {
                span = span.joined(record.range);
                continue;
            }
            if record.range.start() < range.start() {
                if let Ok(prefix) =
                    LockRange::between(record.range.start(), range.start().saturating_sub(1))
                {
                    plan.put(Record {
                        range: prefix,
                        held: record.held,
                    });
                }
            }
            if record.range.end_inclusive() > range.end_inclusive() {
                if let Some(from) = range.end_inclusive().checked_add(1) {
                    if let Ok(suffix) = LockRange::between(from, record.range.end_inclusive()) {
                        plan.put(Record {
                            range: suffix,
                            held: record.held,
                        });
                    }
                }
            }
        }
        if let Some(held) = new {
            plan.put(Record { range: span, held });
        }
        plan
    }

    /// Whether the replace takes out `record`, one touching its range: one
    /// of the new mode is absorbed, and one of the other mode is cut back
    /// where it overlaps and left alone where it merely abuts.
    fn takes(&self, record: Record) -> bool {
        self.new == Some(record.held) || record.range.overlaps(self.range)
    }

    /// Queue `record` to go in; [`PLAN_INSERTS`] is every replace's most.
    fn put(&mut self, record: Record) {
        let pushed = self.insert.try_push(record);
        debug_assert!(pushed.is_ok(), "a replace planned more records than it can");
    }

    /// How many exclusive records the replace puts in.
    fn exclusive_inserts(&self) -> usize {
        self.insert
            .iter()
            .filter(|record| record.held == Held::Exclusive)
            .count()
    }

    /// How many records the owner's count changes by.
    fn delta(&self) -> i64 {
        let added = i64::try_from(self.insert.len()).unwrap_or(i64::MAX);
        let removed = i64::try_from(self.removes).unwrap_or(i64::MAX);
        added - removed
    }
}

/// The first of `records` that `plan` takes out.
fn first_taken(records: &OrderedMap<u64, Record>, plan: &Plan) -> Option<Record> {
    touching(records, plan.range).find(|&record| plan.takes(record))
}

/// The records of one non-overlapping ordered set that overlap `range` or
/// touch it end to end.
///
/// The predecessor is the only record starting before `range` that can reach
/// it — that is what non-overlap buys — so the candidate set is that one
/// plus everything starting at or just past the range, and the walk costs
/// `O(log n)` plus the records it yields.
fn touching(
    records: &OrderedMap<u64, Record>,
    range: LockRange,
) -> impl Iterator<Item = Record> + '_ {
    let predecessor = records
        .range(..range.start())
        .next_back()
        .filter(|(_, record)| record.range.overlaps(range) || record.range.abuts(range))
        .map(|(_, record)| *record);
    let upper = range
        .end_inclusive()
        .checked_add(1)
        .unwrap_or(range.end_inclusive());
    predecessor.into_iter().chain(
        records
            .range(range.start()..=upper)
            .map(|(_, record)| *record)
            .filter(move |record| record.range.overlaps(range) || record.range.abuts(range)),
    )
}

/// The records of one owner that overlap `range`.
fn overlapping(
    records: &OrderedMap<u64, Record>,
    range: LockRange,
) -> impl Iterator<Item = Record> + '_ {
    touching(records, range).filter(move |record| record.range.overlaps(range))
}

/// What a commit installs that did not exist before it, each built with the
/// room its first insertions take.
struct Fresh {
    file: Option<FileEntry>,
    owner: Option<OwnerLocks>,
    owned: Option<OrderedSet<LockedNode>>,
}

impl Registry {
    /// A new entry for `file` when it has none, with room in the registry to
    /// file it; `None` when it has one.
    fn fresh_file(&mut self, file: LockedNode) -> Result<Option<FileEntry>, TryReserveError> {
        if self.files.contains_key(&file) {
            return Ok(None);
        }
        self.files.try_reserve(1)?;
        Ok(Some(FileEntry::new(self.next_key)))
    }

    /// File the entry [`Self::fresh_file`] built, if any, answering `file`'s
    /// wait key.
    fn install_file(&mut self, file: LockedNode, fresh: Option<FileEntry>) -> u64 {
        if let Some(entry) = fresh {
            self.next_key = self.next_key.saturating_add(1);
            filed(self.files.try_insert(file, entry));
        }
        self.files.get(&file).map_or(0, |entry| entry.key)
    }

    /// Take every allocation committing `plan` for `owner` on `file` makes,
    /// changing nothing a reader sees: room in each map an insertion lands
    /// in, and the containers that do not exist yet.
    fn make_room(
        &mut self,
        file: LockedNode,
        owner: OwnerId,
        pid: ProcessId,
        plan: &Plan,
    ) -> Result<Fresh, TryReserveError> {
        let mut entry = self.fresh_file(file)?;
        let locks = match entry.as_mut().or_else(|| self.files.get_mut(&file)) {
            Some(target) => target.make_room(owner, pid, plan)?,
            None => None,
        };
        if plan.delta() > 0 && !self.charged.contains_key(&pid) {
            self.charged.try_reserve(1)?;
        }
        let owned_files = if let Some(files) = self.owned.get_mut(&owner) {
            if !files.contains(&file) {
                files.try_reserve(1)?;
            }
            None
        } else {
            self.owned.try_reserve(1)?;
            Some(OrderedSet::try_with_capacity(1)?)
        };
        Ok(Fresh {
            file: entry,
            owner: locks,
            owned: owned_files,
        })
    }

    /// Install `fresh` and carry out `plan` for `owner` on `file`, in the room
    /// [`Self::make_room`] took — so nothing here can fail.
    fn commit(
        &mut self,
        file: LockedNode,
        owner: OwnerId,
        pid: ProcessId,
        plan: &Plan,
        fresh: Fresh,
    ) {
        self.install_file(file, fresh.file);
        if let Some(entry) = self.files.get_mut(&file) {
            if let Some(locks) = fresh.owner {
                filed(entry.owners.try_insert(owner, locks));
            }
            entry.apply(owner, plan);
        }
        self.settle_charge(pid, plan.delta());
        if let Some(files) = fresh.owned {
            filed(self.owned.try_insert(owner, files));
        }
        if let Some(files) = self.owned.get_mut(&owner) {
            filed(files.try_insert(file));
        }
    }

    /// Forget that `owner` holds records on `file` once it holds none there.
    fn disown_if_released(&mut self, file: LockedNode, owner: OwnerId) {
        let holds = self
            .files
            .get(&file)
            .is_some_and(|entry| entry.owners.contains_key(&owner));
        if holds {
            return;
        }
        if let Some(files) = self.owned.get_mut(&owner) {
            files.remove(&file);
            if files.is_empty() {
                self.owned.remove(&owner);
            }
        }
    }

    /// Drop everything `owner` holds, answering each file's freed waiters —
    /// or `None` when even the list of them cannot be had.
    fn release_all(&mut self, owner: OwnerId) -> Option<Vec<Wakes>> {
        let Some(files) = self.owned.remove(&owner) else {
            return Some(Vec::new());
        };
        let mut wakes = Vec::new();
        let listed = wakes.try_reserve_exact(files.len()).is_ok();
        for file in files {
            let Some(entry) = self.files.get_mut(&file) else {
                continue;
            };
            let Some(locks) = entry.owners.remove(&owner) else {
                continue;
            };
            for &record in locks.records.values() {
                entry.projection.forget(record);
            }
            let freed = i64::try_from(locks.records.len()).unwrap_or(i64::MAX);
            self.settle_charge(locks.pid, -freed);
            if listed {
                let woken = self.freed_waiters(file, |queued| {
                    overlapping(&locks.records, queued).next().is_some()
                });
                if !woken.is_empty() {
                    wakes.push(woken);
                }
            }
            self.drop_if_vacant(file);
        }
        listed.then_some(wakes)
    }

    /// Queue `task`, keeping the arrival order of one already queued, with
    /// every allocation taken first; answers the file's wait key.
    fn join_queue(
        &mut self,
        file: LockedNode,
        owner: OwnerId,
        task: TaskId,
        range: LockRange,
        held: Held,
    ) -> Result<u64, TryReserveError> {
        let entry = self.fresh_file(file)?;
        if !self.waiters.contains_key(&task) {
            self.waiters.try_reserve(1)?;
        }
        let tasks = if let Some(tasks) = self.queued.get_mut(&file) {
            if !tasks.contains(&task) {
                tasks.try_reserve(1)?;
            }
            None
        } else {
            self.queued.try_reserve(1)?;
            Some(OrderedSet::try_with_capacity(1)?)
        };
        let key = self.install_file(file, entry);
        // Re-queuing after a spurious wake keeps the original arrival order,
        // so a waiter that loops never loses its place to a newcomer.
        let seq = if let Some(existing) = self.waiters.get(&task) {
            existing.seq
        } else {
            let seq = self.next_seq;
            self.next_seq = self.next_seq.saturating_add(1);
            seq
        };
        let waiter = Waiter {
            file,
            owner,
            range,
            held,
            seq,
        };
        filed(self.waiters.try_insert(task, waiter));
        if let Some(tasks) = tasks {
            filed(self.queued.try_insert(file, tasks));
        }
        if let Some(tasks) = self.queued.get_mut(&file) {
            filed(tasks.try_insert(task));
        }
        Ok(key)
    }

    /// Refuse a plan that would push `pid` past `limit` records.
    ///
    /// # Errors
    ///
    /// [`Refusal::LimitExceeded`] when the growth does not fit.
    fn price(&self, pid: ProcessId, delta: i64, limit: u64) -> Result<(), Refusal> {
        if delta <= 0 {
            return Ok(());
        }
        let held = self.charged.get(&pid).copied().unwrap_or(0);
        if held.saturating_add(delta.unsigned_abs()) > limit {
            return Err(Refusal::LimitExceeded);
        }
        Ok(())
    }

    /// Charge or credit `delta` records to `pid`, keeping the system-wide
    /// live count in step. A first charge lands in room
    /// [`Self::make_room`] took; a credit never allocates.
    fn settle_charge(&mut self, pid: ProcessId, delta: i64) {
        let magnitude = delta.unsigned_abs();
        if delta > 0 {
            match self.charged.get_mut(&pid) {
                Some(slot) => *slot = slot.saturating_add(magnitude),
                None => filed(self.charged.try_insert(pid, magnitude)),
            }
            LIVE_RECORDS.fetch_add(magnitude, Ordering::Relaxed);
        } else if delta < 0 {
            if let Some(slot) = self.charged.get_mut(&pid) {
                *slot = slot.saturating_sub(magnitude);
                if *slot == 0 {
                    self.charged.remove(&pid);
                }
            }
            let shed = magnitude.min(LIVE_RECORDS.load(Ordering::Relaxed));
            LIVE_RECORDS.fetch_sub(shed, Ordering::Relaxed);
        }
    }

    /// Take `task`'s request out of the queue, answering it.
    fn unqueue(&mut self, task: TaskId) -> Option<Waiter> {
        let waiter = self.waiters.remove(&task)?;
        if let Some(tasks) = self.queued.get_mut(&waiter.file) {
            tasks.remove(&task);
            if tasks.is_empty() {
                self.queued.remove(&waiter.file);
            }
        }
        Some(waiter)
    }

    /// Forget a file nothing holds or awaits, so the registry carries only
    /// files something is actually using.
    fn drop_if_vacant(&mut self, file: LockedNode) {
        if self.queued.contains_key(&file) {
            return;
        }
        if self.files.get(&file).is_some_and(FileEntry::is_vacant) {
            self.files.remove(&file);
        }
    }

    /// The waiters queued on `file` whose blocked ranges `freed` says a change
    /// advances — exactly those, so releasing one range never disturbs a
    /// waiter queued on a disjoint one. When that list cannot be had the
    /// answer is every waiter on the file, each to re-test its own request,
    /// so no change is refused for whom it wakes.
    fn freed_waiters(&self, file: LockedNode, freed: impl Fn(LockRange) -> bool) -> Wakes {
        let Some(entry) = self.files.get(&file) else {
            return Wakes::Nobody;
        };
        let mut tasks = Vec::new();
        for &task in self.queued.get(&file).into_iter().flatten() {
            if !self
                .waiters
                .get(&task)
                .is_some_and(|waiter| freed(waiter.range))
            {
                continue;
            }
            if tasks.try_reserve(1).is_err() {
                return Wakes::File(entry.key);
            }
            tasks.push(task);
        }
        if tasks.is_empty() {
            Wakes::Nobody
        } else {
            Wakes::Named {
                key: entry.key,
                tasks,
            }
        }
    }

    /// Whether an earlier-queued waiter blocks `held` over `range`.
    fn queue_blocked(
        &self,
        file: LockedNode,
        owner: OwnerId,
        range: LockRange,
        held: Held,
        seq: u64,
    ) -> bool {
        self.file_waiters(file).any(|waiter| {
            waiter.owner != owner
                && waiter.seq < seq
                && waiter.range.overlaps(range)
                && !held.compatible_with(waiter.held)
        })
    }

    /// The waiters queued on `file`.
    fn file_waiters(&self, file: LockedNode) -> impl Iterator<Item = &Waiter> {
        self.queued
            .get(&file)
            .into_iter()
            .flatten()
            .filter_map(|task| self.waiters.get(task))
    }

    /// Push every owner `wanting` waits for onto `found`: the holders in its
    /// way, and — unless it converts a range its owner holds — the waiters
    /// queued ahead of it on those bytes, exactly as acquisition decides.
    fn blocking_owners(
        &self,
        wanting: &Waiter,
        found: &mut Vec<OwnerId>,
    ) -> Result<(), alloc::collections::TryReserveError> {
        let Some(entry) = self.files.get(&wanting.file) else {
            return Ok(());
        };
        for (&other, locks) in &entry.owners {
            let blocks = other != wanting.owner
                && overlapping(&locks.records, wanting.range)
                    .any(|record| !wanting.held.compatible_with(record.held));
            if blocks {
                found.try_reserve(1)?;
                found.push(other);
            }
        }
        if entry.holds_any(wanting.owner, wanting.range) {
            return Ok(());
        }
        for waiter in self.file_waiters(wanting.file) {
            let ahead = waiter.owner != wanting.owner
                && waiter.seq < wanting.seq
                && waiter.range.overlaps(wanting.range)
                && !wanting.held.compatible_with(waiter.held);
            if ahead {
                found.try_reserve(1)?;
                found.push(waiter.owner);
            }
        }
        Ok(())
    }

    /// Whether granting `wanting` its wait would close a cycle.
    ///
    /// # Errors
    ///
    /// [`Refusal::OutOfMemory`] when the search cannot hold the owners it
    /// has reached.
    fn would_deadlock(&self, wanting: &Waiter) -> Result<bool, Refusal> {
        let mut visited = OrderedSet::new();
        let mut frontier = Vec::new();
        self.blocking_owners(wanting, &mut frontier)?;
        while let Some(blocker) = frontier.pop() {
            if blocker == wanting.owner {
                return Ok(true);
            }
            if !visited.try_insert(blocker)? {
                continue;
            }
            // What that owner is itself waiting for. Registry-wide because
            // an owner may wait on any file; this runs only for a request
            // that is already blocked, and the waiter set is bounded by the
            // tasks currently parked in `fs_lock`.
            for waiter in self.waiters.values().filter(|w| w.owner == blocker) {
                self.blocking_owners(waiter, &mut frontier)?;
            }
        }
        Ok(false)
    }
}

/// Discard every lock and waiter — a test fixture's reset, so one case
/// cannot observe another's records through the shared registry.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    let mut reg = REGISTRY.lock();
    reg.files.clear();
    reg.owned.clear();
    reg.charged.clear();
    reg.waiters.clear();
    reg.queued.clear();
    LIVE_RECORDS.store(0, Ordering::Relaxed);
}

/// The ranges `owner` holds on `file`, in order — the shape a test asserts
/// the record algebra against.
#[cfg(test)]
pub(crate) fn held_ranges(file: LockedNode, owner: OwnerId) -> Vec<(u64, u64, Held)> {
    let reg = REGISTRY.lock();
    reg.files
        .get(&file)
        .and_then(|entry| entry.owners.get(&owner))
        .map(|locks| {
            locks
                .records
                .values()
                .map(|record| {
                    (
                        record.range.start(),
                        record.range.end_inclusive(),
                        record.held,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the derived indexes still describe the records they project, and
/// the per-owner sets are minimal and non-overlapping — the invariant every
/// operation must preserve.
#[cfg(test)]
pub(crate) fn invariants_hold(file: LockedNode) -> bool {
    let reg = REGISTRY.lock();
    let Some(entry) = reg.files.get(&file) else {
        return true;
    };
    let mut exclusive: Vec<(u64, u64, OwnerId)> = Vec::new();
    let mut shared = 0usize;
    for (&owner, locks) in &entry.owners {
        if locks.records.is_empty() {
            return false;
        }
        let mut previous: Option<Record> = None;
        for (&key, record) in &locks.records {
            if key != record.range.start() {
                return false;
            }
            if let Some(earlier) = previous {
                // Non-overlapping, and never two same-mode records that
                // merely abut — the set would not be minimal.
                if earlier.range.overlaps(record.range) {
                    return false;
                }
                if earlier.range.abuts(record.range) && earlier.held == record.held {
                    return false;
                }
            }
            previous = Some(*record);
            match record.held {
                Held::Shared => shared += 1,
                Held::Exclusive => {
                    exclusive.push((record.range.start(), record.range.end_inclusive(), owner));
                }
            }
        }
    }
    if shared != entry.projection.shared {
        return false;
    }
    let mut projected: Vec<(u64, u64, OwnerId)> = entry
        .projection
        .exclusive
        .iter()
        .map(|(_, &(range, owner))| (range.start(), range.end_inclusive(), owner))
        .collect();
    projected.sort_unstable();
    exclusive.sort_unstable();
    if projected != exclusive {
        return false;
    }
    // Exclusive records are globally non-overlapping, which is what makes
    // the projection an exact interval index.
    projected.windows(2).all(|pair| pair[0].1 < pair[1].0)
}

/// The system-wide live record count — the figure the fast-path check reads.
#[cfg(test)]
pub(crate) fn live_records() -> u64 {
    LIVE_RECORDS.load(Ordering::Relaxed)
}

/// Whether `task` has a waiter row, and whether `file`'s queue names it —
/// both or neither, outside a change. Allocation-free, so a refusal sweep
/// can ask it between runs.
#[cfg(test)]
pub(crate) fn queued_state(file: LockedNode, task: TaskId) -> (bool, bool) {
    let reg = REGISTRY.lock();
    (
        reg.waiters.contains_key(&task),
        reg.queued
            .get(&file)
            .is_some_and(|tasks| tasks.contains(&task)),
    )
}

#[cfg(test)]
#[path = "filelock_tests.rs"]
mod tests;
