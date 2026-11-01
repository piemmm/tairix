//! Storage charged for the room it holds: a context's tables and queues grow
//! only by room charged to it first, and keep that charge for as long as
//! they keep the room, so the peak a context's own requests grew them to is
//! paid for by that context alone.

use alloc::collections::VecDeque;
use core::ops::Deref;

use tairix_collections::{Key, SlotMap};

use crate::memory::Charge;
use crate::model::exhausted;
use crate::Error;

/// The fewest entries a container grows to from none.
const LEAST_ROOM: usize = 4;

/// Storage that holds entries in room it makes ahead of them.
pub(crate) trait Room {
    /// The bytes its room takes.
    fn room_bytes(&self) -> u64;
    /// The bytes room for `more` entries past those it holds would add.
    fn growth_bytes(&self, more: usize) -> u64;
    /// Room for exactly `more` entries past those it holds.
    fn try_grow(&mut self, more: usize) -> Result<(), Error>;
    /// The entries it holds, and those it has room for.
    fn held(&self) -> usize;
    fn room(&self) -> usize;
}

impl<T> Room for VecDeque<T> {
    fn room_bytes(&self) -> u64 {
        (self.capacity() as u64).saturating_mul(size_of::<T>() as u64)
    }

    fn growth_bytes(&self, more: usize) -> u64 {
        let wanted = self.len().saturating_add(more);
        (wanted.saturating_sub(self.capacity()) as u64).saturating_mul(size_of::<T>() as u64)
    }

    fn try_grow(&mut self, more: usize) -> Result<(), Error> {
        self.try_reserve_exact(more).map_err(exhausted)
    }

    fn held(&self) -> usize {
        self.len()
    }

    fn room(&self) -> usize {
        self.capacity()
    }
}

impl<K: Key, V> Room for SlotMap<K, V> {
    fn room_bytes(&self) -> u64 {
        self.held_bytes() as u64
    }

    fn growth_bytes(&self, more: usize) -> u64 {
        self.reserve_bytes(more) as u64
    }

    fn try_grow(&mut self, more: usize) -> Result<(), Error> {
        self.try_reserve_exact(more).map_err(exhausted)
    }

    fn held(&self) -> usize {
        self.len()
    }

    fn room(&self) -> usize {
        self.capacity()
    }
}

/// Storage and the charge for its room, which always covers it.
#[derive(Debug)]
pub(crate) struct Charged<S> {
    storage: S,
    charge: Charge,
}

/// A table of a context's objects, its room charged to the context.
pub(crate) type Table<K, V> = Charged<SlotMap<K, V>>;

impl<S> Deref for Charged<S> {
    type Target = S;

    fn deref(&self) -> &S {
        &self.storage
    }
}

impl<S: Room> Charged<S> {
    /// `storage` under `charge`, refused where the charge falls short of its
    /// room.
    pub(crate) fn new(storage: S, charge: Charge) -> Result<Self, Error> {
        if charge.bytes() < storage.room_bytes() {
            return Err(Error::Malformed);
        }
        Ok(Self { storage, charge })
    }

    /// Where it has no room for another entry, the growth
    /// [`Self::growth_to`] makes for one.
    pub(crate) fn growth(&self) -> Option<(usize, u64)> {
        self.growth_to(self.storage.held().saturating_add(1))
    }

    /// Where it has room for fewer than `room` entries, the entries past
    /// those it holds to grow by so it has room for that many, its room at
    /// least doubled so a run of growths moves it few times, and the bytes
    /// they take.
    pub(crate) fn growth_to(&self, room: usize) -> Option<(usize, u64)> {
        let now = self.storage.room();
        if now >= room {
            return None;
        }
        let wanted = room.max(now.saturating_mul(2)).max(LEAST_ROOM);
        let more = wanted.saturating_sub(self.storage.held());
        Some((more, self.storage.growth_bytes(more)))
    }

    /// Make room for `more` entries past those it holds, under `charge`:
    /// where the room cannot be had, nothing changes and the charge is given
    /// back.
    pub(crate) fn grow(&mut self, more: usize, charge: Charge) -> Result<(), Error> {
        let bytes = charge.bytes();
        self.charge.absorb(charge)?;
        if let Err(error) = self.storage.try_grow(more) {
            self.charge.release(bytes);
            return Err(error);
        }
        Ok(())
    }

    /// Whether another entry fits without growing it.
    pub(crate) fn fits(&self) -> bool {
        self.storage.held() < self.storage.room()
    }
}

impl<K: Key, V> Charged<SlotMap<K, V>> {
    /// File `value` in room already made, answering its key: refused, and
    /// `value` dropped, where there is none.
    pub(crate) fn try_insert(&mut self, value: V) -> Result<K, Error> {
        if !self.fits() {
            return Err(Error::OutOfMemory);
        }
        self.storage.try_insert(value).map_err(exhausted)
    }

    pub(crate) fn get_mut(&mut self, key: K) -> Option<&mut V> {
        self.storage.get_mut(key)
    }

    /// Take the value `key` names out, its slot kept for the next.
    pub(crate) fn remove(&mut self, key: K) -> Option<V> {
        self.storage.remove(key)
    }
}

impl<T> Charged<VecDeque<T>> {
    /// Add `value` at the back, in room already made: refused, and `value`
    /// answered, where there is none.
    pub(crate) fn push_back(&mut self, value: T) -> Result<(), T> {
        if !self.fits() {
            return Err(value);
        }
        self.storage.push_back(value);
        Ok(())
    }

    pub(crate) fn get_mut(&mut self, at: usize) -> Option<&mut T> {
        self.storage.get_mut(at)
    }

    pub(crate) fn pop_front(&mut self) -> Option<T> {
        self.storage.pop_front()
    }

    pub(crate) fn remove(&mut self, at: usize) -> Option<T> {
        self.storage.remove(at)
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.storage.truncate(len);
    }

    pub(crate) fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        self.storage.retain(keep);
    }

    /// Give back room past twice what it holds once it holds no more than a
    /// quarter of it, so a burst's peak is not kept.
    pub(crate) fn settle(&mut self) {
        self.settle_to(self.storage.len());
    }

    /// Give back room past twice `needed` entries, at least those it holds,
    /// once `needed` is no more than a quarter of its room: where the
    /// smaller room cannot be had, the room stays, still charged.
    pub(crate) fn settle_to(&mut self, needed: usize) {
        let room = self.storage.capacity();
        let needed = needed.max(self.storage.len());
        if room <= LEAST_ROOM || needed > room / 4 {
            return;
        }
        let mut smaller = VecDeque::new();
        if smaller
            .try_reserve_exact(needed.saturating_mul(2).max(LEAST_ROOM))
            .is_err()
        {
            return;
        }
        let before = self.storage.room_bytes();
        smaller.extend(self.storage.drain(..));
        self.storage = smaller;
        self.charge
            .release(before.saturating_sub(self.storage.room_bytes()));
    }
}

#[cfg(test)]
#[path = "room_tests.rs"]
mod tests;
