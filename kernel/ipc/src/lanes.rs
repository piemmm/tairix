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

extern crate alloc;

use alloc::vec::Vec;
use core::hash::Hash;

use tairix_abi::Errno;
use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
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

/// A sender's neighbours in the turn order.
#[derive(Copy, Clone, Debug)]
struct Link<K> {
    prev: Option<K>,
    next: Option<K>,
}

struct Entry<K, L> {
    lane: L,
    /// Its place in the turn order, while the sender has traffic waiting.
    turn: Option<Link<K>>,
}

/// What a lane costs the endpoint beyond the items in it: its index slot.
pub(crate) const fn lane_cost<K, L>() -> usize {
    size_of::<(K, Entry<K, L>)>() + 1
}

type Index<K, L> = HashMap<K, Entry<K, L>, BuildFastHash>;

/// Senders are numbers the kernel assigns, never chosen by a sender, so the
/// fast unkeyed hash is safe here.
const fn index<K, L>() -> Index<K, L> {
    HashMap::with_hasher(BuildFastHash::new())
}

/// Every sender's lane at one endpoint, and the order they are served in.
pub(crate) struct Lanes<K, L> {
    entries: Index<K, L>,
    /// The first and the last sender in the turn order.
    turn: Option<(K, K)>,
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
        let mut entries = index();
        entries.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        Ok(Self(entries))
    }
}

impl<K: Copy + Eq + Hash, L: Lane> Lanes<K, L> {
    /// No lanes.
    pub(crate) const fn new() -> Self {
        Self {
            entries: index(),
            turn: None,
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

    fn link_mut(&mut self, sender: K) -> Option<&mut Link<K>> {
        self.entries
            .get_mut(&sender)
            .and_then(|entry| entry.turn.as_mut())
    }

    /// Put `sender` at the back of the turn order, unless it is already in it.
    /// A sender with no lane has nothing to wait for and is left out.
    pub(crate) fn join_turn(&mut self, sender: K) {
        let last = self.turn.map(|(_, last)| last);
        let Some(entry) = self.entries.get_mut(&sender) else {
            return;
        };
        if entry.turn.is_some() {
            return;
        }
        entry.turn = Some(Link {
            prev: last,
            next: None,
        });
        self.turn = match self.turn {
            Some((first, last)) => {
                if let Some(link) = self.link_mut(last) {
                    link.next = Some(sender);
                }
                Some((first, sender))
            }
            None => Some((sender, sender)),
        };
    }

    /// Take `sender` out of the turn order wherever it stands.
    fn unlink(&mut self, sender: K) {
        let Some(link) = self
            .entries
            .get_mut(&sender)
            .and_then(|entry| entry.turn.take())
        else {
            return;
        };
        if let Some(prev) = link.prev.and_then(|prev| self.link_mut(prev)) {
            prev.next = link.next;
        }
        if let Some(next) = link.next.and_then(|next| self.link_mut(next)) {
            next.prev = link.prev;
        }
        self.turn = self.turn.and_then(|(first, last)| {
            let first = if first == sender {
                link.next
            } else {
                Some(first)
            };
            let last = if last == sender {
                link.prev
            } else {
                Some(last)
            };
            first.zip(last)
        });
    }

    /// The sender served next.
    pub(crate) fn next_turn(&self) -> Option<K> {
        self.turn.map(|(first, _)| first)
    }

    /// End the turn [`Self::next_turn`] named: a sender with more waiting goes
    /// to the back, one with nothing left leaves the order.
    pub(crate) fn end_turn(&mut self, still_waiting: bool) {
        let Some((first, last)) = self.turn else {
            return;
        };
        if still_waiting && first == last {
            return;
        }
        self.unlink(first);
        if still_waiting {
            self.join_turn(first);
        }
    }

    /// Take `sender` out of the turn order wherever it stands: its last
    /// waiting item went some other way than being served.
    pub(crate) fn leave_turn(&mut self, sender: K) {
        self.unlink(sender);
    }

    /// Whether any sender has traffic waiting.
    pub(crate) fn has_turn(&self) -> bool {
        self.turn.is_some()
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
        self.unlink(sender);
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
        self.unlink(sender);
        self.entries.remove(&sender).map(|entry| entry.lane)
    }

    /// Keep only `keep`'s lane, its place in the turn order with it, moving
    /// every other into what is returned for the caller to drop outside its
    /// lock. Allocates nothing: `room` was made beforehand.
    pub(crate) fn keep_only(&mut self, keep: K, room: KeepRoom<K, L>) -> Retired<K, L> {
        let mut others = core::mem::replace(&mut self.entries, room.0);
        let kept = others.remove(&keep);
        let in_turn = kept.as_ref().is_some_and(|entry| entry.turn.is_some());
        if let Some(mut entry) = kept {
            entry.turn = in_turn.then_some(Link {
                prev: None,
                next: None,
            });
            // The room was reserved, so this cannot allocate; a refusal drops
            // the lane rather than leave it half kept.
            let _ = self.entries.try_insert(keep, entry);
        }
        self.turn = (in_turn && self.entries.contains_key(&keep)).then_some((keep, keep));
        Retired {
            entries: others,
            _spare: Vec::new(),
        }
    }

    /// Take every lane, spares included, for teardown.
    pub(crate) fn take_all(&mut self) -> Retired<K, L> {
        self.turn = None;
        Retired {
            entries: core::mem::replace(&mut self.entries, index()),
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

/// Who to wake once a sender's lane has room: the threads waiting as that
/// sender, or, when an endpoint ends, every thread waiting at it.
#[derive(Debug)]
pub struct RoomWake<K> {
    waiting: Waiting<K>,
}

#[derive(Debug)]
enum Waiting<K> {
    Nobody,
    Sender(Vec<u64>),
    Everyone(HashMap<K, Vec<u64>, BuildFastHash>),
}

impl<K> RoomWake<K> {
    /// Nobody to wake.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            waiting: Waiting::Nobody,
        }
    }

    /// Whether nobody needs waking.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks().next().is_none()
    }

    /// Every scheduler task to wake.
    pub fn tasks(&self) -> impl Iterator<Item = u64> + '_ {
        let sender = match &self.waiting {
            Waiting::Sender(tasks) => tasks.as_slice(),
            Waiting::Nobody | Waiting::Everyone(_) => &[],
        };
        let everyone = match &self.waiting {
            Waiting::Everyone(groups) => Some(groups.values().flatten()),
            Waiting::Nobody | Waiting::Sender(_) => None,
        };
        sender.iter().chain(everyone.into_iter().flatten()).copied()
    }
}

/// Tasks parked until a sender's lane has room, grouped by the sender they
/// wait as, so waking one sender's waiters never touches another's.
pub(crate) struct RoomWaiters<K> {
    groups: HashMap<K, Vec<u64>, BuildFastHash>,
}

impl<K: Copy + Eq + Hash> RoomWaiters<K> {
    /// Nobody waiting.
    pub(crate) const fn new() -> Self {
        Self {
            groups: HashMap::with_hasher(BuildFastHash::new()),
        }
    }

    /// Record `task` as parked for room in `sender`'s lane. Idempotent.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the record cannot be kept; the task must
    /// then not park on a wake that would never come.
    pub(crate) fn watch(&mut self, task: u64, sender: K) -> Result<(), Errno> {
        if let Some(group) = self.groups.get_mut(&sender) {
            if !group.contains(&task) {
                group.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
                group.push(task);
            }
            return Ok(());
        }
        let mut group = Vec::new();
        group.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        group.push(task);
        self.groups
            .try_insert(sender, group)
            .map(drop)
            .map_err(|_| Errno::OutOfMemory)
    }

    /// Forget `task`'s wait as `sender`, returning an emptied group for the
    /// caller to drop outside its lock.
    pub(crate) fn unwatch(&mut self, task: u64, sender: K) -> Option<Vec<u64>> {
        let group = self.groups.get_mut(&sender)?;
        group.retain(|&waiting| waiting != task);
        if group.is_empty() {
            self.groups.remove(&sender)
        } else {
            None
        }
    }

    /// The tasks waiting as `sender`, whose lane just gained room. Taken, so
    /// each wait is woken once; one that still finds no room records itself
    /// again.
    pub(crate) fn take_waiting_as(&mut self, sender: K) -> RoomWake<K> {
        RoomWake {
            waiting: self
                .groups
                .remove(&sender)
                .map_or(Waiting::Nobody, Waiting::Sender),
        }
    }

    /// Every waiting task, for a change that touches every lane at once.
    pub(crate) fn take_everyone(&mut self) -> RoomWake<K> {
        if self.groups.is_empty() {
            return RoomWake::none();
        }
        RoomWake {
            waiting: Waiting::Everyone(core::mem::replace(
                &mut self.groups,
                HashMap::with_hasher(BuildFastHash::new()),
            )),
        }
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
    fn room_waiters_wake_only_the_tasks_waiting_as_a_sender_and_each_once() {
        let mut waiters = RoomWaiters::new();
        waiters.watch(1, 7).expect("recorded");
        waiters.watch(2, 9).expect("recorded");
        waiters.watch(3, 9).expect("recorded");
        waiters
            .watch(3, 9)
            .expect("recording twice changes nothing");
        assert!(waiters.take_waiting_as(5).is_empty());
        assert_eq!(tasks(&waiters.take_waiting_as(9)), [2, 3]);
        assert!(waiters.take_waiting_as(9).is_empty(), "woken once");
        assert_eq!(tasks(&waiters.take_waiting_as(7)), [1]);
    }

    #[test]
    fn however_many_wait_as_one_sender_each_is_named() {
        let mut waiters = RoomWaiters::new();
        for task in 0..100 {
            waiters.watch(task, 4).expect("recorded");
        }
        waiters.watch(500, 8).expect("recorded");
        let woken = waiters.take_waiting_as(4);
        assert_eq!(tasks(&woken), (0..100).collect::<Vec<u64>>());
        assert_eq!(tasks(&waiters.take_everyone()), [500]);
    }

    #[test]
    fn a_forgotten_wait_is_not_woken_and_an_emptied_group_goes() {
        let mut waiters = RoomWaiters::new();
        waiters.watch(1, 7).expect("recorded");
        waiters.watch(2, 7).expect("recorded");
        assert!(waiters.unwatch(1, 7).is_none(), "the group still waits");
        assert!(waiters.unwatch(9, 7).is_none());
        assert!(waiters.unwatch(2, 7).is_some(), "the emptied group goes");
        assert!(waiters.take_everyone().is_empty());
        assert!(RoomWake::<u64>::none().is_empty());
    }
}
