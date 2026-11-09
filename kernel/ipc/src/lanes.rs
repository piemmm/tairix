//! Per-sender lanes: how an endpoint many senders share keeps each one's
//! traffic apart, bounds it by a depth the owner declares, and serves the
//! lanes in turn.
//!
//! A single total every sender drew from let any one of them fill an endpoint
//! and refuse every other; with a lane each, a sender's own traffic is the only
//! thing that can refuse it, and a sender with a deep backlog waits its turn
//! behind one item from each of the others rather than ahead of all of them.
//!
//! Finding, opening, serving and retiring a lane cost the same however many
//! senders share the endpoint: lanes are found by hash, and the turn order is a
//! list threaded through them. Only opening a lane allocates, and fallibly;
//! serving a turn, retiring a lane and teardown never do, so the receive and
//! reply paths cannot fail for memory.
//!
//! A thread waiting for room in its sender's lane waits in that sender's line
//! ([`RoomWaiters`]). A slot that frees goes to the first thread in the line,
//! which holds it until it uses it or leaves, when it passes to the next: one
//! wake per slot, never the whole line, and never a slot free while a thread
//! waits for it.

extern crate alloc;

use alloc::vec::Vec;
use core::hash::Hash;

use tairix_abi::Errno;
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_inline::ArrayVec;
use tairix_kernel_sec::Charge;

/// What one sender holds at an endpoint.
pub(crate) trait Lane {
    /// Whether the sender holds nothing here, so its lane can be retired.
    fn is_idle(&self) -> bool;

    /// Whether an idle lane's storage is small enough to keep for the next
    /// sender rather than hand back to the heap.
    fn is_reusable(&self) -> bool;

    /// Return what the sender pays for the lane, which becomes a spare the
    /// endpoint's owner holds.
    fn release(&mut self) -> Option<Charge>;
}

/// Idle lanes an endpoint keeps, so a sender arriving after another left takes
/// its storage instead of allocating.
pub(crate) const SPARE_LANES: usize = 4;

/// The most items an idle lane may have room for and still be kept as a
/// spare: enough for the request/reply rhythm every client has, small enough
/// that a burst cannot leave a deep lane retained.
pub(crate) const SPARE_LANE_ITEMS: usize = 4;

/// A member's neighbours in an [`Order`].
#[derive(Copy, Clone, Debug)]
pub(crate) struct Link<K> {
    prev: Option<K>,
    next: Option<K>,
}

/// A value holding its own place in an [`Order`] threaded through its map.
pub(crate) trait Linked<K> {
    /// Its place, while it is in the order.
    fn link(&mut self) -> &mut Option<Link<K>>;
}

/// The maps lanes and waits are kept in. Their keys are numbers the kernel
/// assigns, never chosen by a sender, so the fast unkeyed hash is safe.
type Map<K, V> = HashMap<K, V, BuildFastHash>;

const fn map<K, V>() -> Map<K, V> {
    HashMap::with_hasher(BuildFastHash::new())
}

/// A first-in, first-out order threaded through the values of a map, each
/// holding its neighbours: joining at the back, leaving from anywhere and
/// finding the front cost the same however many are in it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Order<K> {
    /// The first and the last member.
    ends: Option<(K, K)>,
    len: usize,
}

impl<K: Copy + Eq + Hash> Order<K> {
    /// Nobody in order.
    pub(crate) const fn new() -> Self {
        Self { ends: None, len: 0 }
    }

    /// The member at the front.
    pub(crate) fn first(&self) -> Option<K> {
        self.ends.map(|(first, _)| first)
    }

    /// How many are in order.
    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    /// Whether nobody is in order.
    pub(crate) const fn is_empty(&self) -> bool {
        self.ends.is_none()
    }

    /// Put `member` at the back, unless it is in order already or `map` holds
    /// no value for it.
    pub(crate) fn join<V: Linked<K>>(&mut self, map: &mut Map<K, V>, member: K) {
        let last = self.ends.map(|(_, last)| last);
        let Some(link) = map.get_mut(&member).map(Linked::link) else {
            return;
        };
        if link.is_some() {
            return;
        }
        *link = Some(Link {
            prev: last,
            next: None,
        });
        if let Some(back) = last.and_then(|last| link_of(map, last)) {
            back.next = Some(member);
        }
        self.ends = Some((self.first().unwrap_or(member), member));
        self.len += 1;
    }

    /// Take `member` out of the order wherever it stands.
    pub(crate) fn leave<V: Linked<K>>(&mut self, map: &mut Map<K, V>, member: K) {
        let Some(link) = map.get_mut(&member).and_then(|value| value.link().take()) else {
            return;
        };
        if let Some(prev) = link.prev.and_then(|prev| link_of(map, prev)) {
            prev.next = link.next;
        }
        if let Some(next) = link.next.and_then(|next| link_of(map, next)) {
            next.prev = link.prev;
        }
        self.ends = self.ends.and_then(|(first, last)| {
            let first = if first == member {
                link.next
            } else {
                Some(first)
            };
            let last = if last == member {
                link.prev
            } else {
                Some(last)
            };
            first.zip(last)
        });
        self.len -= 1;
    }
}

/// The place `member`'s value holds in an [`Order`] threaded through `map`.
fn link_of<K: Copy + Eq + Hash, V: Linked<K>>(
    map: &mut Map<K, V>,
    member: K,
) -> Option<&mut Link<K>> {
    map.get_mut(&member)?.link().as_mut()
}

struct Entry<K, L> {
    lane: L,
    /// Its place in the turn order, while the sender has traffic waiting.
    turn: Option<Link<K>>,
}

impl<K, L> Linked<K> for Entry<K, L> {
    fn link(&mut self) -> &mut Option<Link<K>> {
        &mut self.turn
    }
}

/// What a lane costs the endpoint beyond the items in it: its index slot.
pub(crate) const fn lane_cost<K, L>() -> usize {
    size_of::<(K, Entry<K, L>)>() + 1
}

type Index<K, L> = Map<K, Entry<K, L>>;

/// Every sender's lane at one endpoint, and the order they are served in.
pub(crate) struct Lanes<K, L> {
    entries: Index<K, L>,
    /// The senders with traffic waiting, in the order they are served.
    turn: Order<K>,
    spare: Vec<L>,
}

/// What [`Lanes::open`] found or made.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Opened {
    /// The sender had a lane already.
    Held,
    /// The sender was given a spare or the fresh lane.
    Made,
    /// Neither a spare nor a fresh lane was at hand: the caller makes one
    /// outside its lock and opens again.
    NeedsFresh,
}

/// Lanes taken out, dropped by their holder outside its lock.
pub(crate) struct Retired<K, L> {
    entries: Index<K, L>,
    _spare: Vec<L>,
}

impl<K: Copy, L> Retired<K, L> {
    /// Every retired lane with its sender.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (K, &L)> {
        self.entries
            .iter()
            .map(|(sender, entry)| (*sender, &entry.lane))
    }

    /// Drop each retired lane, telling `released` its sender once what the
    /// lane held is given back.
    pub(crate) fn release_each(self, mut released: impl FnMut(K)) {
        for (sender, entry) in self.entries {
            drop(entry);
            released(sender);
        }
    }
}

/// What retiring a lane leaves for its holder to drop outside its lock.
#[must_use = "dropped outside the lock it was taken under"]
pub(crate) enum Retiring<L> {
    /// Nothing was retired.
    Nothing,
    /// Kept as a spare; what its sender paid for it is returned.
    Kept {
        /// Returned to the sender when dropped.
        _paid: Option<Charge>,
    },
    /// Handed back to the heap.
    Dropped {
        /// Freed when dropped.
        _lane: L,
    },
}

/// An index with room for one lane, made outside the lock for
/// [`Lanes::keep_only`].
pub(crate) struct KeepRoom<K, L>(Index<K, L>);

impl<K: Copy + Eq + Hash, L> KeepRoom<K, L> {
    /// Room for one lane.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the room cannot be allocated.
    pub(crate) fn new() -> Result<Self, Errno> {
        let mut entries = map();
        entries.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        Ok(Self(entries))
    }
}

impl<K: Copy + Eq + Hash, L: Lane> Lanes<K, L> {
    /// No lanes.
    pub(crate) const fn new() -> Self {
        Self {
            entries: map(),
            turn: Order::new(),
            spare: Vec::new(),
        }
    }

    /// Room for [`SPARE_LANES`] spares, so retiring a lane never allocates.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the room cannot be allocated.
    pub(crate) fn try_reserve_spares(&mut self) -> Result<(), Errno> {
        self.spare
            .try_reserve_exact(SPARE_LANES)
            .map_err(|_| Errno::OutOfMemory)
    }

    /// `sender`'s lane, if it holds anything here.
    pub(crate) fn get(&self, sender: K) -> Option<&L> {
        self.entries.get(&sender).map(|entry| &entry.lane)
    }

    /// `sender`'s lane, mutably.
    pub(crate) fn get_mut(&mut self, sender: K) -> Option<&mut L> {
        self.entries.get_mut(&sender).map(|entry| &mut entry.lane)
    }

    /// Give `sender` a lane: the one it has, else a spare, else `fresh`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the index cannot grow; nothing is taken.
    pub(crate) fn open(&mut self, sender: K, fresh: &mut Option<L>) -> Result<Opened, Errno> {
        if self.entries.contains_key(&sender) {
            return Ok(Opened::Held);
        }
        if self.spare.is_empty() && fresh.is_none() {
            return Ok(Opened::NeedsFresh);
        }
        self.entries
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        let Some(lane) = self.spare.pop().or_else(|| fresh.take()) else {
            return Ok(Opened::NeedsFresh);
        };
        self.entries
            .try_insert(sender, Entry { lane, turn: None })
            .map_err(|_| Errno::OutOfMemory)?;
        Ok(Opened::Made)
    }

    /// Put `sender` at the back of the turn order, unless it is already in it.
    /// A sender with no lane has nothing to wait for and is left out.
    pub(crate) fn join_turn(&mut self, sender: K) {
        self.turn.join(&mut self.entries, sender);
    }

    /// The sender served next.
    pub(crate) fn next_turn(&self) -> Option<K> {
        self.turn.first()
    }

    /// End the turn [`Self::next_turn`] named: a sender with more waiting goes
    /// to the back, one with nothing left leaves the order.
    pub(crate) fn end_turn(&mut self, still_waiting: bool) {
        let Some(first) = self.turn.first() else {
            return;
        };
        if still_waiting && self.turn.len() == 1 {
            return;
        }
        self.turn.leave(&mut self.entries, first);
        if still_waiting {
            self.turn.join(&mut self.entries, first);
        }
    }

    /// Take `sender` out of the turn order wherever it stands: its last
    /// waiting item went some other way than being served.
    pub(crate) fn leave_turn(&mut self, sender: K) {
        self.turn.leave(&mut self.entries, sender);
    }

    /// Whether any sender has traffic waiting.
    pub(crate) fn has_turn(&self) -> bool {
        !self.turn.is_empty()
    }

    /// Retire `sender`'s lane if it holds nothing: kept as a spare while there
    /// is room and it is small, else handed back for the caller to drop outside
    /// its lock.
    pub(crate) fn retire_if_idle(&mut self, sender: K) -> Retiring<L> {
        if !self
            .entries
            .get(&sender)
            .is_some_and(|entry| entry.lane.is_idle())
        {
            return Retiring::Nothing;
        }
        self.turn.leave(&mut self.entries, sender);
        let Some(Entry { mut lane, .. }) = self.entries.remove(&sender) else {
            return Retiring::Nothing;
        };
        if lane.is_reusable() && self.spare.len() < self.spare.capacity().min(SPARE_LANES) {
            let paid = lane.release();
            self.spare.push(lane);
            Retiring::Kept { _paid: paid }
        } else {
            Retiring::Dropped { _lane: lane }
        }
    }

    /// Remove `sender`'s lane whatever it holds, for the caller to drop
    /// outside its lock.
    pub(crate) fn remove(&mut self, sender: K) -> Option<L> {
        self.turn.leave(&mut self.entries, sender);
        self.entries.remove(&sender).map(|entry| entry.lane)
    }

    /// Keep only `keep`'s lane, its place in the turn order with it, moving
    /// every other into what is returned for the caller to drop outside its
    /// lock. Allocates nothing: `room` was made beforehand.
    pub(crate) fn keep_only(&mut self, keep: K, room: KeepRoom<K, L>) -> Retired<K, L> {
        let mut others = core::mem::replace(&mut self.entries, room.0);
        self.turn = Order::new();
        if let Some(mut entry) = others.remove(&keep) {
            let in_turn = entry.turn.take().is_some();
            // The room was reserved, so this cannot allocate; a refusal drops
            // the lane rather than leave it half kept.
            let _ = self.entries.try_insert(keep, entry);
            if in_turn {
                self.turn.join(&mut self.entries, keep);
            }
        }
        Retired {
            entries: others,
            _spare: Vec::new(),
        }
    }

    /// Take every lane, spares included, for teardown.
    pub(crate) fn take_all(&mut self) -> Retired<K, L> {
        self.turn = Order::new();
        Retired {
            entries: core::mem::replace(&mut self.entries, map()),
            _spare: core::mem::take(&mut self.spare),
        }
    }

    /// Every lane with its sender, in no particular order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (K, &L)> {
        self.entries
            .iter()
            .map(|(sender, entry)| (*sender, &entry.lane))
    }

    /// Lanes held now.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Idle lanes kept for reuse.
    #[cfg(test)]
    pub(crate) fn spares(&self) -> usize {
        self.spare.len()
    }
}

/// Who an endpoint's change leaves to be woken once its lock is released: the
/// thread a freed slot was handed to, a sender's watchers, or, when an
/// endpoint ends, every thread waiting at it.
///
/// A structure that could not record a waiter for want of memory leaves that
/// waiter to a broadcast on its wait-queue instead, which the next wake it
/// gives carries ([`Self::tasks`] names it as task `0`).
#[derive(Debug)]
pub struct RoomWake<K> {
    waiting: Waiting<K>,
    broadcast: bool,
}

#[derive(Debug)]
enum Waiting<K> {
    Nobody,
    /// The thread a freed slot was handed to.
    One(u64),
    /// A sender's watchers, copied out so its group keeps its room.
    Sender(ArrayVec<u64, WATCH_BATCH>),
    /// A sender's watchers, more than a batch holds, with their group's room.
    Crowd(Vec<u64>),
    /// Every watcher at an endpoint that went.
    Everyone(Map<K, Vec<u64>>),
    /// Every thread waiting for room at an endpoint that went, or whose every
    /// lane changed at once.
    All(RoomWaiters<K>),
}

impl<K> RoomWake<K> {
    /// Nobody to wake.
    #[must_use]
    pub const fn none() -> Self {
        Self::of(Waiting::Nobody)
    }

    const fn of(waiting: Waiting<K>) -> Self {
        Self {
            waiting,
            broadcast: false,
        }
    }

    /// This wake, carrying a broadcast as well when `owed`.
    fn broadcast_if(mut self, owed: bool) -> Self {
        self.broadcast |= owed;
        self
    }

    /// Whether nobody needs waking.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks().next().is_none()
    }

    /// Every scheduler task to wake.
    pub fn tasks(&self) -> impl Iterator<Item = u64> + '_ {
        let one = match self.waiting {
            Waiting::One(task) => Some(task),
            _ => None,
        };
        let sender = match &self.waiting {
            Waiting::Sender(tasks) => tasks.as_slice(),
            Waiting::Crowd(tasks) => tasks.as_slice(),
            _ => &[],
        };
        let everyone = match &self.waiting {
            Waiting::Everyone(groups) => Some(groups.values().flatten()),
            _ => None,
        };
        let all = match &self.waiting {
            Waiting::All(waiters) => Some(waiters.records.keys()),
            _ => None,
        };
        one.into_iter()
            .chain(sender.iter().copied())
            .chain(everyone.into_iter().flatten().copied())
            .chain(all.into_iter().flatten().copied())
            .chain(self.broadcast.then_some(0))
    }
}

/// How a thread waiting for room in its sender's lane stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Standing {
    /// In its sender's line for the next slot to free.
    Waiting,
    /// A freed slot was handed to it, and it holds that slot until it uses or
    /// leaves it.
    Designated,
    /// Its wait reported the room ready, and it holds the slot until its next
    /// call that is neither a send nor a wait watching the port, or its end.
    /// Ports only.
    Owing,
}

/// One thread's wait for room.
#[derive(Debug)]
struct Record<K> {
    sender: K,
    standing: Standing,
    /// Its place in its sender's line, or among the threads holding a slot.
    place: Option<Link<u64>>,
}

impl<K> Linked<u64> for Record<K> {
    fn link(&mut self) -> &mut Option<Link<u64>> {
        &mut self.place
    }
}

/// One sender's threads waiting for room, and those holding a slot.
#[derive(Debug)]
struct Line {
    waiting: Order<u64>,
    holding: Order<u64>,
}

/// The threads waiting for room in their senders' lanes, one line a sender,
/// kept under the lock that guards the lanes.
///
/// A slot that frees is handed to the first thread in its sender's line, which
/// holds it — no sibling's send takes it — until it uses or leaves it, when it
/// passes to the next. So each freed slot wakes one thread, and at every
/// release of the lock each free slot is held by one thread or has no thread
/// waiting for it: a thread parks only after looking and finding none it may
/// take. Every operation costs the same however many wait.
#[derive(Debug)]
pub(crate) struct RoomWaiters<K> {
    /// Each waiting thread's record, by scheduler task.
    records: Map<u64, Record<K>>,
    lines: Map<K, Line>,
    /// A waiter went unrecorded for want of memory, and the next room handed
    /// out is broadcast.
    overflowed: bool,
}

impl<K: Copy + Eq + Hash> RoomWaiters<K> {
    /// Nobody waiting.
    pub(crate) const fn new() -> Self {
        Self {
            records: map(),
            lines: map(),
            overflowed: false,
        }
    }

    /// The slots `sender`'s threads hold.
    fn held(&self, sender: K) -> usize {
        self.lines.get(&sender).map_or(0, |of| of.holding.len())
    }

    /// The first thread in `sender`'s line.
    fn first_waiting(&self, sender: K) -> Option<u64> {
        self.lines.get(&sender)?.waiting.first()
    }

    /// The sender `task` waits as.
    pub(crate) fn sender_of(&self, task: u64) -> Option<K> {
        self.records.get(&task).map(|record| record.sender)
    }

    /// Whether `task` holds a slot.
    pub(crate) fn holds(&self, task: u64) -> bool {
        self.records
            .get(&task)
            .is_some_and(|record| record.standing != Standing::Waiting)
    }

    /// Whether a send by `task` as `sender`, with `free` slots free in its
    /// lane, finds one: its own, or one no thread of `sender` holds.
    pub(crate) fn has_room(&self, task: u64, sender: K, free: usize) -> bool {
        self.holds(task) || free > self.held(sender)
    }

    /// Look for room as `task`, a thread of `sender`, with `free` slots free in
    /// its lane, answering whether it holds one now: a slot no thread holds is
    /// handed to it. Otherwise it waits in `sender`'s line, keeping its place
    /// there if it waited before.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when its record cannot be kept: it must then not
    /// park, since nothing would wake it.
    pub(crate) fn look(&mut self, task: u64, sender: K, free: usize) -> bool {
        if !self.records.contains_key(&task) && self.wait(task, sender).is_err() {
            self.overflow();
            return false;
        }
        let Some(record) = self.records.get(&task) else {
            return false;
        };
        let (waits_as, standing) = (record.sender, record.standing);
        if standing != Standing::Waiting {
            return true;
        }
        if free > self.held(waits_as) {
            self.designate(task);
            return true;
        }
        false
    }

    /// A waiter could not be recorded: it is woken by the broadcast the next
    /// room handed out carries, rather than refused its wait.
    pub(crate) fn overflow(&mut self) {
        self.overflowed = true;
    }

    /// Put `task` at the back of `sender`'s line.
    fn wait(&mut self, task: u64, sender: K) -> Result<(), Errno> {
        self.records
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        if !self.lines.contains_key(&sender) {
            let line = Line {
                waiting: Order::new(),
                holding: Order::new(),
            };
            self.lines
                .try_insert(sender, line)
                .map_err(|_| Errno::OutOfMemory)?;
        }
        let record = Record {
            sender,
            standing: Standing::Waiting,
            place: None,
        };
        // Reserved above, so this cannot allocate.
        let _ = self.records.try_insert(task, record);
        if let Some(line) = self.lines.get_mut(&sender) {
            line.waiting.join(&mut self.records, task);
        }
        Ok(())
    }

    /// Hand `task`, waiting, the slot it waits for.
    fn designate(&mut self, task: u64) {
        let Some(sender) = self.sender_of(task) else {
            return;
        };
        let Some(line) = self.lines.get_mut(&sender) else {
            return;
        };
        line.waiting.leave(&mut self.records, task);
        line.holding.join(&mut self.records, task);
        if let Some(record) = self.records.get_mut(&task) {
            record.standing = Standing::Designated;
        }
    }

    /// A slot freed in `sender`'s lane, which now has `free`: hand one no
    /// thread holds to the first thread in its line, answering whom to wake.
    /// With nobody waiting this is one look at an empty map.
    pub(crate) fn hand_out(&mut self, sender: K, free: usize) -> RoomWake<K> {
        let broadcast = core::mem::take(&mut self.overflowed);
        if self.records.is_empty() || free <= self.held(sender) {
            return RoomWake::none().broadcast_if(broadcast);
        }
        let Some(first) = self.first_waiting(sender) else {
            return RoomWake::none().broadcast_if(broadcast);
        };
        self.designate(first);
        RoomWake::of(Waiting::One(first)).broadcast_if(broadcast)
    }

    /// `task` sent into the slot it holds, which ends its wait. Answers
    /// whether it held one.
    pub(crate) fn consume(&mut self, task: u64) -> bool {
        self.holds(task) && self.remove(task).is_some()
    }

    /// End `task`'s wait, answering its sender when a slot it held is let go
    /// of, for the caller to hand on ([`Self::hand_out`]).
    pub(crate) fn leave(&mut self, task: u64) -> Option<K> {
        match self.remove(task)? {
            (sender, Standing::Designated | Standing::Owing) => Some(sender),
            (_, Standing::Waiting) => None,
        }
    }

    /// Keep the slot `task` holds now its wait reported the room ready, until
    /// its next call that is neither a send nor a wait watching the port, or
    /// its end; a thread holding none takes one of the `free` no thread holds.
    /// Answers whether it holds a slot; one that does not ends its wait.
    pub(crate) fn own(&mut self, task: u64, free: usize) -> bool {
        let Some(record) = self.records.get(&task) else {
            return false;
        };
        let (sender, standing) = (record.sender, record.standing);
        if standing == Standing::Waiting {
            if free <= self.held(sender) {
                let _ = self.remove(task);
                return false;
            }
            self.designate(task);
        }
        if let Some(record) = self.records.get_mut(&task) {
            record.standing = Standing::Owing;
        }
        true
    }

    /// Forget every record of `sender`, which has ended: its threads are gone.
    pub(crate) fn take_sender(&mut self, sender: K) {
        let Some(line) = self.lines.remove(&sender) else {
            return;
        };
        for first in [line.waiting.first(), line.holding.first()] {
            let mut next = first;
            while let Some(task) = next {
                next = self
                    .records
                    .remove(&task)
                    .and_then(|record| record.place)
                    .and_then(|link| link.next);
            }
        }
    }

    /// Every record, for a change that touches every lane at once.
    pub(crate) fn take_everyone(&mut self) -> RoomWake<K> {
        let broadcast = core::mem::take(&mut self.overflowed);
        if self.records.is_empty() {
            return RoomWake::none().broadcast_if(broadcast);
        }
        RoomWake::of(Waiting::All(core::mem::replace(self, Self::new()))).broadcast_if(broadcast)
    }

    /// Drop `task`'s record from its line, answering its sender and standing.
    fn remove(&mut self, task: u64) -> Option<(K, Standing)> {
        let record = self.records.get(&task)?;
        let (sender, standing) = (record.sender, record.standing);
        let emptied = self.lines.get_mut(&sender).is_some_and(|line| {
            match standing {
                Standing::Waiting => line.waiting.leave(&mut self.records, task),
                Standing::Designated | Standing::Owing => {
                    line.holding.leave(&mut self.records, task);
                }
            }
            line.waiting.is_empty() && line.holding.is_empty()
        });
        if emptied {
            self.lines.remove(&sender);
        }
        self.records.remove(&task);
        Some((sender, standing))
    }
}

/// Watchers one answer wakes while their group keeps its room, so the next
/// watch needs no memory; a larger group hands its list over whole.
const WATCH_BATCH: usize = 16;

/// Tasks watching for the answers their sender is owed, grouped by that
/// sender, so an answer wakes its own sender's watchers and no other's. A
/// watch is found by its task, so recording and forgetting one cost the same
/// however many watch.
///
/// A group outlives its watches until its sender ends or the endpoint goes,
/// so a thread that watches again and again needs memory only the first time.
pub(crate) struct Watchers<K> {
    /// Each watching task's sender, and its place in that sender's group.
    records: Map<u64, (K, usize)>,
    groups: Map<K, Vec<u64>>,
    /// A watcher went unrecorded for want of memory, and the next answer is
    /// broadcast.
    overflowed: bool,
}

impl<K: Copy + Eq + Hash> Watchers<K> {
    /// Nobody watching.
    pub(crate) const fn new() -> Self {
        Self {
            records: map(),
            groups: map(),
            overflowed: false,
        }
    }

    /// A watcher could not be recorded: it is woken by the broadcast the next
    /// answer carries, rather than refused its wait.
    pub(crate) fn overflow(&mut self) {
        self.overflowed = true;
    }

    /// Record `task` as watching for `sender`'s answers, idempotently; one the
    /// heap has no room to record is left to a broadcast ([`Self::overflow`]).
    pub(crate) fn watch(&mut self, task: u64, sender: K) {
        if self.record(task, sender).is_err() {
            self.overflow();
        }
    }

    fn record(&mut self, task: u64, sender: K) -> Result<(), Errno> {
        if self.records.contains_key(&task) {
            return Ok(());
        }
        self.records
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        if let Some(group) = self.groups.get_mut(&sender) {
            group.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        } else {
            let mut group = Vec::new();
            group.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
            self.groups
                .try_insert(sender, group)
                .map_err(|_| Errno::OutOfMemory)?;
        }
        let Some(group) = self.groups.get_mut(&sender) else {
            return Err(Errno::OutOfMemory);
        };
        let at = group.len();
        group.push(task);
        // Reserved above, so this cannot allocate.
        let _ = self.records.try_insert(task, (sender, at));
        Ok(())
    }

    /// Whether `task` watches for `sender`'s answers.
    fn watches(&self, task: u64, sender: K) -> bool {
        self.records
            .get(&task)
            .is_some_and(|&(watching, _)| watching == sender)
    }

    /// Forget `task`'s watch as `sender`.
    pub(crate) fn unwatch(&mut self, task: u64, sender: K) {
        if !self.watches(task, sender) {
            return;
        }
        let Some((_, at)) = self.records.remove(&task) else {
            return;
        };
        let Some(group) = self.groups.get_mut(&sender) else {
            return;
        };
        if at < group.len() {
            let _ = group.swap_remove(at);
        }
        let moved = group.get(at).copied();
        if let Some(record) = moved.and_then(|moved| self.records.get_mut(&moved)) {
            record.1 = at;
        }
    }

    /// The watchers an answer to a call of `sender`'s wakes, taken so each is
    /// woken once: none when one of `named`, the threads the call names and
    /// its answer wakes already, watches for it, and else every watcher of
    /// `sender`, since a thread other than its poster may collect it. One that
    /// still finds nothing records itself again.
    pub(crate) fn take_answered(
        &mut self,
        sender: K,
        mut named: impl Iterator<Item = u64>,
    ) -> RoomWake<K> {
        let broadcast = core::mem::take(&mut self.overflowed);
        if self.groups.is_empty() || named.any(|task| self.watches(task, sender)) {
            return RoomWake::none().broadcast_if(broadcast);
        }
        self.take_watching_as(sender).broadcast_if(broadcast)
    }

    /// The tasks watching for `sender`'s answers, taken, so each watch is
    /// woken once, for an answer no thread its call names watches for. The
    /// group stays, with its room, for the next watch.
    pub(crate) fn take_watching_as(&mut self, sender: K) -> RoomWake<K> {
        let broadcast = core::mem::take(&mut self.overflowed);
        let Self {
            records, groups, ..
        } = self;
        let Some(group) = groups.get_mut(&sender) else {
            return RoomWake::none().broadcast_if(broadcast);
        };
        for task in group.iter() {
            records.remove(task);
        }
        let waiting = if group.len() <= WATCH_BATCH {
            let mut batch = ArrayVec::new();
            for task in group.drain(..) {
                let copied = batch.try_push(task);
                debug_assert!(copied.is_ok(), "a group past its batch was copied");
            }
            Waiting::Sender(batch)
        } else {
            Waiting::Crowd(core::mem::take(group))
        };
        RoomWake::of(waiting).broadcast_if(broadcast)
    }

    /// The tasks watching for the answers of `sender`, which has ended, taken
    /// with its group.
    pub(crate) fn take_ended(&mut self, sender: K) -> RoomWake<K> {
        let broadcast = core::mem::take(&mut self.overflowed);
        let Some(group) = self.groups.remove(&sender) else {
            return RoomWake::none().broadcast_if(broadcast);
        };
        for task in &group {
            self.records.remove(task);
        }
        RoomWake::of(Waiting::Crowd(group)).broadcast_if(broadcast)
    }

    /// Every watching task, for an endpoint that went.
    pub(crate) fn take_everyone(&mut self) -> RoomWake<K> {
        let broadcast = core::mem::take(&mut self.overflowed);
        if self.groups.is_empty() {
            return RoomWake::none().broadcast_if(broadcast);
        }
        // Keeps its table, so nothing is freed under the endpoint's lock.
        self.records.clear();
        RoomWake::of(Waiting::Everyone(core::mem::replace(
            &mut self.groups,
            map(),
        )))
        .broadcast_if(broadcast)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lane holding a count of items, reusable while it was never deep.
    #[derive(Debug, Default, Eq, PartialEq)]
    struct Count {
        items: usize,
        deep: bool,
        released: usize,
    }

    impl Lane for Count {
        fn is_idle(&self) -> bool {
            self.items == 0
        }

        fn is_reusable(&self) -> bool {
            !self.deep
        }

        fn release(&mut self) -> Option<Charge> {
            self.released += 1;
            None
        }
    }

    fn lanes() -> Lanes<u64, Count> {
        let mut lanes = Lanes::new();
        lanes.try_reserve_spares().expect("spares");
        lanes
    }

    fn open(lanes: &mut Lanes<u64, Count>, sender: u64) {
        let mut fresh = Some(Count::default());
        assert!(matches!(
            lanes.open(sender, &mut fresh),
            Ok(Opened::Held | Opened::Made)
        ));
    }

    fn served(lanes: &mut Lanes<u64, Count>, rounds: impl Fn(u64, usize) -> bool) -> Vec<u64> {
        let mut served = Vec::new();
        while let Some(sender) = lanes.next_turn() {
            served.push(sender);
            let times = served.iter().filter(|&&s| s == sender).count();
            lanes.end_turn(rounds(sender, times));
        }
        served
    }

    #[test]
    fn a_lane_is_made_from_a_spare_before_a_fresh_one_and_never_twice() {
        let mut lanes = lanes();
        let mut none = None;
        assert_eq!(lanes.open(7, &mut none), Ok(Opened::NeedsFresh));
        let mut fresh = Some(Count::default());
        assert_eq!(lanes.open(7, &mut fresh), Ok(Opened::Made));
        assert!(fresh.is_none(), "the fresh lane was taken");
        let mut another = Some(Count::default());
        assert_eq!(lanes.open(7, &mut another), Ok(Opened::Held));
        assert!(another.is_some(), "an existing lane takes nothing");
        assert_eq!(lanes.len(), 1);
        assert!(matches!(
            lanes.retire_if_idle(7),
            Retiring::Kept { _paid: None }
        ));
        assert_eq!(lanes.spares(), 1);
        assert_eq!(
            lanes.open(9, &mut none),
            Ok(Opened::Made),
            "a spare needs no fresh"
        );
        assert_eq!(lanes.spares(), 0);
        assert_eq!(lanes.get(9).map(|lane| lane.released), Some(1));
    }

    #[test]
    fn lanes_are_found_by_sender_whatever_order_they_open_in() {
        let mut lanes = lanes();
        for sender in [30, 10, 20, 40] {
            open(&mut lanes, sender);
            lanes.get_mut(sender).expect("opened").items = usize::try_from(sender).expect("small");
        }
        for sender in [10, 20, 30, 40] {
            assert_eq!(
                lanes.get(sender).map(|lane| lane.items),
                usize::try_from(sender).ok()
            );
        }
        let mut senders: Vec<u64> = lanes.iter().map(|(sender, _)| sender).collect();
        senders.sort_unstable();
        assert_eq!(senders, [10, 20, 30, 40]);
        assert!(lanes.get(25).is_none());
    }

    #[test]
    fn the_turn_serves_each_sender_once_a_round_and_rotates_the_ones_still_waiting() {
        let mut lanes = lanes();
        for sender in [1, 2, 3] {
            open(&mut lanes, sender);
            lanes.join_turn(sender);
            lanes.join_turn(sender);
        }
        // Sender 1 has two rounds of work, the others one.
        let order = served(&mut lanes, |sender, times| sender == 1 && times < 2);
        assert_eq!(order, [1, 2, 3, 1]);
        assert!(!lanes.has_turn());
    }

    #[test]
    fn a_lone_sender_still_waiting_keeps_its_turn() {
        let mut lanes = lanes();
        open(&mut lanes, 4);
        lanes.join_turn(4);
        let order = served(&mut lanes, |_, times| times < 3);
        assert_eq!(order, [4, 4, 4]);
    }

    #[test]
    fn a_sender_leaving_out_of_turn_is_forgotten_by_the_order_wherever_it_stood() {
        for leaving in [1, 2, 3] {
            let mut lanes = lanes();
            for sender in [1, 2, 3] {
                open(&mut lanes, sender);
                lanes.join_turn(sender);
            }
            lanes.leave_turn(leaving);
            lanes.leave_turn(leaving);
            let expected: Vec<u64> = [1, 2, 3].into_iter().filter(|&s| s != leaving).collect();
            assert_eq!(served(&mut lanes, |_, _| false), expected, "{leaving}");
            lanes.join_turn(leaving);
            assert_eq!(lanes.next_turn(), Some(leaving), "it may join again");
        }
    }

    #[test]
    fn a_sender_without_a_lane_never_joins_the_turn() {
        let mut lanes = lanes();
        lanes.join_turn(5);
        assert!(!lanes.has_turn());
    }

    #[test]
    fn only_an_idle_reusable_lane_is_kept_and_only_as_many_as_the_spares() {
        let mut lanes = lanes();
        open(&mut lanes, 1);
        lanes.get_mut(1).expect("opened").items = 1;
        assert!(
            matches!(lanes.retire_if_idle(1), Retiring::Nothing),
            "a busy lane stays"
        );
        assert_eq!(lanes.len(), 1);
        open(&mut lanes, 2);
        lanes.get_mut(2).expect("opened").deep = true;
        assert!(
            matches!(lanes.retire_if_idle(2), Retiring::Dropped { .. }),
            "a deep lane is handed back, not kept"
        );
        let senders = 10..10 + SPARE_LANES as u64 + 2;
        for sender in senders.clone() {
            open(&mut lanes, sender);
        }
        let handed_back = senders
            .filter(|&sender| matches!(lanes.retire_if_idle(sender), Retiring::Dropped { .. }))
            .count();
        assert_eq!(lanes.spares(), SPARE_LANES);
        assert_eq!(
            handed_back, 2,
            "past the spares, a lane goes back to the heap"
        );
    }

    #[test]
    fn a_retired_lane_leaves_the_turn() {
        let mut lanes = lanes();
        for sender in [1, 2] {
            open(&mut lanes, sender);
            lanes.join_turn(sender);
        }
        assert!(matches!(lanes.retire_if_idle(1), Retiring::Kept { .. }));
        assert_eq!(served(&mut lanes, |_, _| false), [2]);
    }

    #[test]
    fn removal_takes_a_lane_and_its_turn_whatever_it_holds() {
        let mut lanes = lanes();
        for sender in [1, 2] {
            open(&mut lanes, sender);
            lanes.get_mut(sender).expect("opened").items = 3;
            lanes.join_turn(sender);
        }
        assert_eq!(lanes.remove(1).map(|lane| lane.items), Some(3));
        assert_eq!(lanes.next_turn(), Some(2));
        assert!(lanes.remove(1).is_none());
    }

    #[test]
    fn keeping_one_lane_moves_every_other_out_and_keeps_its_turn() {
        let mut lanes = lanes();
        for sender in [1, 2, 3] {
            open(&mut lanes, sender);
            lanes.get_mut(sender).expect("opened").items = 1;
            lanes.join_turn(sender);
        }
        let others = lanes.keep_only(2, KeepRoom::new().expect("room"));
        let mut gone: Vec<u64> = others.iter().map(|(sender, _)| sender).collect();
        gone.sort_unstable();
        assert_eq!(gone, [1, 3]);
        assert_eq!(lanes.len(), 1);
        assert_eq!(served(&mut lanes, |_, _| false), [2]);

        let out_of_turn = {
            let mut lanes = Lanes::<u64, Count>::new();
            open(&mut lanes, 5);
            let _ = lanes.keep_only(5, KeepRoom::new().expect("room"));
            lanes
        };
        assert!(out_of_turn.get(5).is_some());
        assert!(
            !out_of_turn.has_turn(),
            "a lane not in turn is not put in it"
        );

        let absent = lanes.keep_only(9, KeepRoom::new().expect("room"));
        assert_eq!(absent.iter().count(), 1);
        assert_eq!(lanes.len(), 0);
        assert!(!lanes.has_turn());
    }

    #[test]
    fn teardown_takes_every_lane() {
        let mut lanes = lanes();
        for sender in [1, 2, 3] {
            open(&mut lanes, sender);
            lanes.join_turn(sender);
        }
        let retired = lanes.take_all();
        assert_eq!(retired.iter().count(), 3);
        assert_eq!(lanes.len(), 0);
        assert!(!lanes.has_turn());
    }

    /// The order survives any mix of joins, services, departures and
    /// retirements: it serves exactly the senders still waiting, each once a
    /// round, the oldest join first.
    #[test]
    fn the_turn_order_matches_a_queue_model_under_any_mix_of_operations() {
        let mut rng = tairix_fuzzseed::Prng::new(0x1a7e_5eed_0bde_7a11);
        for _ in 0..200 {
            let mut lanes = Lanes::<u64, Count>::new();
            let mut model: alloc::collections::VecDeque<u64> = alloc::collections::VecDeque::new();
            for _ in 0..64 {
                let draw = rng.next_u64();
                let sender = draw % 6;
                match (draw >> 8) % 5 {
                    0 | 1 => {
                        open(&mut lanes, sender);
                        lanes.get_mut(sender).expect("opened").items = 1;
                        lanes.join_turn(sender);
                        if !model.contains(&sender) {
                            model.push_back(sender);
                        }
                    }
                    2 => {
                        let still = (draw >> 16).is_multiple_of(2);
                        assert_eq!(lanes.next_turn(), model.front().copied());
                        lanes.end_turn(still);
                        if let Some(first) = model.pop_front() {
                            if still {
                                model.push_back(first);
                            }
                        }
                    }
                    3 => {
                        lanes.leave_turn(sender);
                        model.retain(|&queued| queued != sender);
                    }
                    _ => {
                        if let Some(lane) = lanes.get_mut(sender) {
                            lane.items = 0;
                        }
                        let _ = lanes.retire_if_idle(sender);
                        model.retain(|&queued| queued != sender);
                    }
                }
                assert_eq!(lanes.next_turn(), model.front().copied());
                assert_eq!(lanes.has_turn(), !model.is_empty());
            }
            let rest: Vec<u64> = model.iter().copied().collect();
            assert_eq!(served(&mut lanes, |_, _| false), rest);
        }
    }

    fn tasks(wake: &RoomWake<u64>) -> Vec<u64> {
        let mut tasks: Vec<u64> = wake.tasks().collect();
        tasks.sort_unstable();
        tasks
    }

    #[test]
    fn watchers_wake_only_the_tasks_watching_as_a_sender_and_each_once() {
        let mut watchers = Watchers::new();
        watchers.watch(1, 7);
        watchers.watch(2, 9);
        watchers.watch(3, 9);
        watchers.watch(3, 9);
        assert!(watchers.take_watching_as(5).is_empty());
        assert_eq!(tasks(&watchers.take_watching_as(9)), [2, 3]);
        assert!(watchers.take_watching_as(9).is_empty(), "woken once");
        assert_eq!(tasks(&watchers.take_watching_as(7)), [1]);
    }

    #[test]
    fn however_many_watch_as_one_sender_each_is_named() {
        let mut watchers = Watchers::new();
        for task in 0..100 {
            watchers.watch(task, 4);
        }
        watchers.watch(500, 8);
        let woken = watchers.take_watching_as(4);
        assert_eq!(tasks(&woken), (0..100).collect::<Vec<u64>>());
        assert_eq!(tasks(&watchers.take_everyone()), [500]);
    }

    #[test]
    fn a_forgotten_watch_is_not_woken_and_an_ended_sender_takes_its_group() {
        let mut watchers = Watchers::new();
        watchers.watch(1, 7);
        watchers.watch(2, 7);
        watchers.unwatch(1, 7);
        watchers.unwatch(9, 7);
        assert_eq!(tasks(&watchers.take_ended(7)), [2]);
        assert!(watchers.groups.is_empty(), "the ended sender's group went");
        assert!(watchers.take_everyone().is_empty());
        assert!(RoomWake::<u64>::none().is_empty());
    }

    /// A thread watching, being woken and watching again keeps reusing its
    /// group's room, so its watches after the first need no memory.
    ///
    /// Regression: an emptied or woken group was dropped and the next watch
    /// allocated a fresh one, so every call-reply wait allocated and a wait
    /// on an exhausted heap was refused at once.
    #[test]
    fn a_watch_after_the_first_needs_no_memory() {
        let mut watchers = Watchers::new();
        watchers.watch(1, 7);
        let held = |watchers: &Watchers<u64>| {
            let group = watchers.groups.get(&7).expect("the group stays");
            (
                group.as_ptr(),
                group.capacity(),
                watchers.records.allocated_bytes(),
                watchers.groups.allocated_bytes(),
            )
        };
        let first = held(&watchers);
        for _ in 0..3 {
            assert_eq!(tasks(&watchers.take_watching_as(7)), [1]);
            assert_eq!(held(&watchers), first, "woken, the group kept its room");
            watchers.watch(1, 7);
            watchers.unwatch(1, 7);
            assert_eq!(held(&watchers), first, "forgotten, the group kept its room");
            watchers.watch(1, 7);
            assert_eq!(held(&watchers), first, "watching again took no memory");
        }
    }

    /// A group larger than one batch is woken whole, every watcher once.
    #[test]
    fn a_crowd_past_one_batch_is_woken_whole() {
        let mut watchers = Watchers::new();
        let crowd = u64::try_from(WATCH_BATCH).expect("a small batch") + 5;
        for task in 0..crowd {
            watchers.watch(task, 4);
        }
        assert_eq!(
            tasks(&watchers.take_watching_as(4)),
            (0..crowd).collect::<Vec<u64>>()
        );
        assert!(watchers.take_watching_as(4).is_empty(), "woken once");
        watchers.watch(3, 4);
        assert_eq!(tasks(&watchers.take_watching_as(4)), [3]);
    }

    /// Forgetting watches in any order leaves every other where a later look
    /// finds it, however its group was reordered.
    #[test]
    fn forgetting_watches_in_any_order_keeps_the_rest_found() {
        let mut watchers = Watchers::new();
        for task in 0..8 {
            watchers.watch(task, 7);
        }
        for task in [0, 7, 3, 4] {
            watchers.unwatch(task, 7);
        }
        for task in [1, 2, 5, 6] {
            assert!(watchers.watches(task, 7), "{task}");
        }
        watchers.unwatch(6, 7);
        assert_eq!(tasks(&watchers.take_watching_as(7)), [1, 2, 5]);
    }

    /// An answer whose call names a watching thread wakes no other watcher of
    /// its sender and leaves their watches standing; one whose call names
    /// none of them wakes them all, once.
    #[test]
    fn an_answer_wakes_its_senders_watchers_only_when_its_call_names_none() {
        let mut watchers = Watchers::new();
        for task in [1, 2, 3] {
            watchers.watch(task, 7);
        }
        watchers.watch(4, 9);
        assert!(
            watchers.take_answered(7, [2, 8].into_iter()).is_empty(),
            "its poster watches"
        );
        assert_eq!(
            tasks(&watchers.take_answered(7, [8].into_iter())),
            [1, 2, 3]
        );
        assert!(
            watchers.take_answered(7, [8].into_iter()).is_empty(),
            "taken once"
        );
        assert_eq!(
            tasks(&watchers.take_everyone()),
            [4],
            "another sender's stand"
        );
    }

    /// Each of `tasks` looks for room as `sender` with nothing free, in turn.
    fn waiting(waiters: &mut RoomWaiters<u64>, sender: u64, tasks: &[u64]) {
        for &task in tasks {
            assert!(!waiters.look(task, sender, 0), "{task}");
        }
    }

    #[test]
    fn a_freed_slot_wakes_the_first_in_line_and_no_other() {
        let mut waiters = RoomWaiters::new();
        waiting(&mut waiters, 7, &[1, 2, 3]);
        waiting(&mut waiters, 9, &[4]);
        assert_eq!(tasks(&waiters.hand_out(7, 1)), [1], "one slot, one wake");
        assert!(waiters.hand_out(7, 1).is_empty(), "the slot is held");
        assert!(waiters.has_room(1, 7, 1), "by the thread it was handed to");
        assert!(!waiters.has_room(2, 7, 1), "and by no sibling");
        assert_eq!(tasks(&waiters.hand_out(7, 2)), [2], "a second, the next");
        assert!(
            waiters.hand_out(9, 0).is_empty(),
            "nothing free, none woken"
        );
    }

    #[test]
    fn a_slot_left_unused_passes_to_the_next_in_line() {
        let mut waiters = RoomWaiters::new();
        waiting(&mut waiters, 7, &[1, 2, 3]);
        assert_eq!(tasks(&waiters.hand_out(7, 1)), [1]);
        assert_eq!(waiters.leave(2), None, "a waiting thread held nothing");
        assert_eq!(waiters.leave(1), Some(7), "the holder let it go");
        assert_eq!(tasks(&waiters.hand_out(7, 1)), [3]);
        assert_eq!(waiters.leave(5), None, "a thread never waiting holds none");
    }

    #[test]
    fn a_rewatch_keeps_its_place_in_line() {
        let mut waiters = RoomWaiters::new();
        waiting(&mut waiters, 7, &[1, 2, 1, 2]);
        assert_eq!(tasks(&waiters.hand_out(7, 1)), [1]);
        assert_eq!(tasks(&waiters.hand_out(7, 2)), [2]);
    }

    #[test]
    fn a_used_slot_ends_the_wait_and_a_free_one_is_taken_by_a_look() {
        let mut waiters = RoomWaiters::new();
        assert!(waiters.look(1, 7, 1), "a slot nobody holds");
        assert!(!waiters.look(2, 7, 1), "held, so the next waits");
        assert!(waiters.consume(1));
        assert!(!waiters.consume(2), "a waiting thread holds none to use");
        assert_eq!(tasks(&waiters.hand_out(7, 1)), [2]);
        assert!(!waiters.has_room(1, 7, 1), "its wait ended with its send");
    }

    #[test]
    fn an_owed_slot_is_held_until_its_holder_sends_or_leaves() {
        let mut waiters = RoomWaiters::new();
        waiting(&mut waiters, 7, &[1, 2]);
        let _ = waiters.hand_out(7, 1);
        assert!(waiters.own(1, 1), "the handed slot is kept");
        assert!(!waiters.has_room(2, 7, 1), "from its siblings");
        assert!(waiters.consume(1), "until it sends");
        assert!(
            !waiters.own(2, 0),
            "a thread with nothing free owes nothing"
        );
        assert!(waiters.hand_out(7, 1).is_empty(), "and its wait ended");
        waiting(&mut waiters, 7, &[3, 4]);
        let _ = waiters.hand_out(7, 1);
        assert!(waiters.own(3, 1));
        assert_eq!(waiters.leave(3), Some(7), "or until it leaves");
        assert_eq!(tasks(&waiters.hand_out(7, 1)), [4]);
    }

    #[test]
    fn an_ended_sender_leaves_no_record_and_a_teardown_takes_every_one() {
        let mut waiters = RoomWaiters::new();
        waiting(&mut waiters, 7, &[1, 2]);
        waiting(&mut waiters, 9, &[3]);
        let _ = waiters.hand_out(7, 1);
        waiters.take_sender(7);
        assert_eq!(waiters.sender_of(1), None);
        assert_eq!(waiters.sender_of(2), None);
        assert!(waiters.hand_out(7, 2).is_empty());
        assert_eq!(tasks(&waiters.take_everyone()), [3]);
        assert!(waiters.take_everyone().is_empty());
    }

    /// At every step each free slot is held by one thread or waited for by
    /// none, however looks, sends, departures, owes and frees interleave.
    #[test]
    fn every_free_slot_is_held_or_unwaited_for_under_any_mix_of_operations() {
        const DEPTH: usize = 2;
        let mut rng = tairix_fuzzseed::Prng::new(0x900d_5107_0bde_7a11);
        for _ in 0..200 {
            let mut waiters = RoomWaiters::new();
            let mut free = 0usize;
            for _ in 0..64 {
                let draw = rng.next_u64();
                let task = draw % 4 + 1;
                match (draw >> 8) % 5 {
                    0 => {
                        let _ = waiters.look(task, 7, free);
                    }
                    1 if free > 0 && waiters.has_room(task, 7, free) => {
                        let _ = waiters.consume(task);
                        free -= 1;
                    }
                    2 => {
                        if let Some(sender) = waiters.leave(task) {
                            let _ = waiters.hand_out(sender, free);
                        }
                    }
                    3 if free < DEPTH => {
                        free += 1;
                        let _ = waiters.hand_out(7, free);
                    }
                    4 => {
                        let _ = waiters.own(task, free);
                    }
                    _ => {}
                }
                let held = waiters.held(7);
                assert!(held <= free, "{held} held of {free} free");
                let waiting = waiters
                    .lines
                    .get(&7)
                    .is_some_and(|line| !line.waiting.is_empty());
                assert!(
                    free == held || !waiting,
                    "a free slot unheld while one waits"
                );
            }
        }
    }
}
