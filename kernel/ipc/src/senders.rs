//! Who uses which endpoint: the index a registry keeps of the endpoints each
//! sender uses, and the filter each endpoint keeps to note a sender there
//! once, so a sender's end visits the endpoints it used and no other.
//!
//! A note is made at a sender's first use of an endpoint and goes when the
//! sender ends or the endpoint does, so the index holds only pairs that both
//! live. The filter remembers under its endpoint's own lock whom it has
//! noted, so a sender coming back takes no other lock.
//!
//! The index's lock is a leaf: an endpoint notes under its own lock, nothing
//! but the heap is taken under the index's, and a sender's end and an
//! endpoint's destruction take and forget with no endpoint lock held.

extern crate alloc;

use alloc::sync::Arc;
use core::hash::Hash;

use tairix_abi::Errno;
use tairix_collections::{HashMap, HashSet};
use tairix_hash::{BuildFastHash, BuildSipHash13};

use crate::port::EndpointId;

/// A set of endpoints. Endpoint ids are numbers their binders choose, so it
/// hashes keyed.
pub type Endpoints = HashSet<EndpointId, BuildSipHash13>;

/// The endpoints each process uses, or owns.
///
/// Processes are numbers the kernel draws, never chosen by a sender, so they
/// hash fast and unkeyed. A process's set goes with its last endpoint.
pub struct EndpointSets<K> {
    sets: HashMap<K, Endpoints, BuildFastHash>,
}

impl<K> EndpointSets<K> {
    /// No sets.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sets: HashMap::with_hasher(BuildFastHash::new()),
        }
    }
}

impl<K> Default for EndpointSets<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Copy + Eq + Hash> EndpointSets<K> {
    /// Add `endpoint` to `process`'s set.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the set cannot grow; nothing is added.
    pub fn insert(&mut self, process: K, endpoint: EndpointId) -> Result<(), Errno> {
        let added = if let Some(set) = self.sets.get_mut(&process) {
            set.try_insert(endpoint).is_ok()
        } else {
            // A boot that never got the per-boot key hashes unkeyed, as the
            // futex table does.
            let keyed = BuildSipHash13::keyed().unwrap_or(BuildSipHash13::UNKEYED);
            let mut set = Endpoints::with_hasher(keyed);
            set.try_insert(endpoint).is_ok() && self.sets.try_insert(process, set).is_ok()
        };
        added.then_some(()).ok_or(Errno::OutOfMemory)
    }

    /// Take `endpoint` out of `process`'s set, handing back the set it emptied
    /// for the caller to drop outside its lock.
    pub fn remove(&mut self, process: K, endpoint: EndpointId) -> Option<Endpoints> {
        let set = self.sets.get_mut(&process)?;
        set.remove(&endpoint);
        if set.is_empty() {
            self.sets.remove(&process)
        } else {
            None
        }
    }

    /// Take `process`'s whole set.
    pub fn take(&mut self, process: K) -> Option<Endpoints> {
        self.sets.remove(&process)
    }

    /// Whether `process` has a set: it uses, or owns, an endpoint.
    #[must_use]
    pub fn holds(&self, process: K) -> bool {
        self.sets.contains_key(&process)
    }
}

/// The endpoints each sender uses, shared by a registry with every endpoint it
/// holds.
pub struct SenderIndex<K> {
    used: tairix_sync::SpinLock<EndpointSets<K>>,
    /// Acquisitions of `used`, for the tests that bound them.
    #[cfg(test)]
    taken: core::sync::atomic::AtomicUsize,
}

impl<K> SenderIndex<K> {
    /// An empty index.
    #[must_use]
    pub fn new() -> Self {
        Self {
            used: tairix_sync::SpinLock::new(EndpointSets::new()),
            #[cfg(test)]
            taken: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn lock(&self) -> tairix_sync::SpinLockGuard<'_, EndpointSets<K>> {
        #[cfg(test)]
        self.taken
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        self.used.lock()
    }
}

impl<K> Default for SenderIndex<K> {
    fn default() -> Self {
        Self::new()
    }
}

/// Sets a going endpoint's forgetting may empty under one acquisition of an
/// index's lock, dropped once it is released.
const FORGET_BATCH: usize = 16;

impl<K: Copy + Eq + Hash> SenderIndex<K> {
    /// Note that `sender` uses `endpoint`.
    fn note(&self, sender: K, endpoint: EndpointId) -> Result<(), Errno> {
        self.lock().insert(sender, endpoint)
    }

    /// Forget each of `senders`' use of `endpoint`, which is going, under one
    /// acquisition of the index's lock for each [`FORGET_BATCH`] of the sets
    /// the forgetting empties, which are dropped with the lock released.
    fn forget(&self, senders: impl IntoIterator<Item = K>, endpoint: EndpointId) {
        let mut senders = senders.into_iter().peekable();
        while senders.peek().is_some() {
            let mut emptied: [Option<Endpoints>; FORGET_BATCH] = Default::default();
            {
                let mut used = self.lock();
                for slot in &mut emptied {
                    *slot = senders
                        .by_ref()
                        .find_map(|sender| used.remove(sender, endpoint));
                    if slot.is_none() {
                        break;
                    }
                }
            }
            drop(emptied);
        }
    }

    /// Every endpoint `sender`, which has ended, used.
    pub fn take(&self, sender: K) -> Option<Endpoints> {
        self.lock().take(sender)
    }

    /// The endpoints `sender` is noted as using, in order.
    #[cfg(test)]
    pub(crate) fn uses(&self, sender: K) -> alloc::vec::Vec<EndpointId> {
        let mut uses = alloc::vec::Vec::new();
        if let Some(set) = self.lock().sets.get(&sender) {
            uses.extend(set.iter().copied());
        }
        uses.sort_unstable();
        uses
    }
}

/// What an endpoint keeps under its own lock so each sender is noted in its
/// registry's index once: whom it has noted, and the index, once the endpoint
/// is registered.
pub(crate) struct Noted<K> {
    /// Senders are numbers the kernel draws, so the fast unkeyed hash is safe.
    senders: HashSet<K, BuildFastHash>,
    index: Option<Arc<SenderIndex<K>>>,
}

impl<K: Copy + Eq + Hash> Noted<K> {
    /// No one noted, and no index to note in.
    pub(crate) const fn new() -> Self {
        Self {
            senders: HashSet::with_hasher(BuildFastHash::new()),
            index: None,
        }
    }

    /// Note each sender in `index` from now on, unless an index was given
    /// already.
    pub(crate) fn note_in(&mut self, index: Arc<SenderIndex<K>>) {
        if self.index.is_none() {
            self.index = Some(index);
        }
    }

    /// Note `sender`'s use of `endpoint` in the index, once. A `closed`
    /// endpoint notes no one: its destruction forgets its senders.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the note cannot be kept; nothing is noted.
    pub(crate) fn note(
        &mut self,
        sender: K,
        endpoint: EndpointId,
        closed: bool,
    ) -> Result<(), Errno> {
        let Some(index) = &self.index else {
            return Ok(());
        };
        if closed || self.senders.contains(&sender) {
            return Ok(());
        }
        self.senders
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        index.note(sender, endpoint)?;
        // Reserved above, so this cannot fail.
        let _ = self.senders.try_insert(sender);
        Ok(())
    }

    /// `sender` has ended: should its number be drawn again, it is noted
    /// afresh.
    pub(crate) fn sender_ended(&mut self, sender: K) {
        self.senders.remove(&sender);
    }

    /// Whom it has noted, taken out under the endpoint's lock as the endpoint
    /// goes, to [`forget`](Self::forget) once the lock is released.
    #[must_use]
    pub(crate) fn take(&mut self) -> Self {
        let emptied = Self {
            index: self.index.clone(),
            ..Self::new()
        };
        core::mem::replace(self, emptied)
    }

    /// Forget in the index every sender noted as using `endpoint`, which is
    /// going.
    pub(crate) fn forget(self, endpoint: EndpointId) {
        if let Some(index) = &self.index {
            index.forget(self.senders.iter().copied(), endpoint);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process's set holds each endpoint once and goes with its last one.
    #[test]
    fn a_set_goes_with_its_last_endpoint() {
        let mut sets = EndpointSets::new();
        for endpoint in [1, 2, 2] {
            sets.insert(7_u64, EndpointId(endpoint)).expect("added");
        }
        sets.insert(8, EndpointId(1)).expect("added");
        assert!(sets.remove(7, EndpointId(1)).is_none(), "one is left");
        assert!(sets.remove(7, EndpointId(9)).is_none());
        let emptied = sets.remove(7, EndpointId(2)).expect("emptied");
        assert!(emptied.is_empty());
        assert!(!sets.holds(7), "its set went");
        assert!(sets.take(7).is_none());
        assert!(sets.holds(8));
        assert_eq!(sets.take(8).map(|set| set.len()), Some(1));
    }

    /// A filter notes each sender once until it ends, and no one before it
    /// has an index or while its endpoint is closed.
    #[test]
    fn a_filter_notes_each_sender_once_until_it_ends() {
        let index: Arc<SenderIndex<u64>> = Arc::new(SenderIndex::new());
        let mut noted = Noted::new();
        noted.note(7, EndpointId(1), false).expect("no index");
        noted.note_in(Arc::clone(&index));
        assert!(index.uses(7).is_empty(), "none before the index");
        noted.note(7, EndpointId(1), false).expect("noted");
        assert_eq!(index.uses(7), [EndpointId(1)]);
        // Taken behind the filter's back, so a second note would show.
        drop(index.take(7));
        noted.note(7, EndpointId(1), false).expect("filtered");
        assert!(index.uses(7).is_empty(), "a sender is noted once");
        noted.sender_ended(7);
        noted.note(7, EndpointId(1), false).expect("noted afresh");
        assert_eq!(index.uses(7), [EndpointId(1)], "afresh once ended");
        noted.note(8, EndpointId(1), true).expect("closed");
        assert!(index.uses(8).is_empty(), "none while closed");
    }

    /// An endpoint going forgets the senders it noted, and only its notes.
    #[test]
    fn a_going_endpoint_forgets_only_its_own_notes() {
        let index: Arc<SenderIndex<u64>> = Arc::new(SenderIndex::new());
        let (mut going, mut staying) = (Noted::new(), Noted::new());
        going.note_in(Arc::clone(&index));
        staying.note_in(Arc::clone(&index));
        for sender in [7, 8] {
            going.note(sender, EndpointId(1), false).expect("noted");
        }
        staying.note(7, EndpointId(2), false).expect("noted");
        going.take().forget(EndpointId(1));
        assert_eq!(index.uses(7), [EndpointId(2)]);
        assert!(index.uses(8).is_empty());
    }

    /// An endpoint going forgets every sender it noted under one acquisition
    /// of the index's lock, however many it noted, but for one more each batch
    /// of sets the forgetting empties: those are dropped with the lock
    /// released.
    ///
    /// Regression: every set a forgetting emptied was dropped under the lock.
    #[test]
    fn a_going_endpoint_forgets_its_senders_and_drops_what_it_empties_unlocked() {
        let index: Arc<SenderIndex<u64>> = Arc::new(SenderIndex::new());
        let (mut first, mut second) = (Noted::new(), Noted::new());
        first.note_in(Arc::clone(&index));
        second.note_in(Arc::clone(&index));
        for sender in 0..64 {
            first.note(sender, EndpointId(1), false).expect("noted");
            second.note(sender, EndpointId(2), false).expect("noted");
        }
        let taken = || index.taken.load(core::sync::atomic::Ordering::Relaxed);
        let before = taken();
        first.take().forget(EndpointId(1));
        assert_eq!(taken() - before, 1, "every set kept, one acquisition");
        let before = taken();
        second.take().forget(EndpointId(2));
        assert_eq!(
            taken() - before,
            64 / FORGET_BATCH,
            "and one a batch of the sets it empties"
        );
        assert!((0..64).all(|sender| index.uses(sender).is_empty()));
    }
}
