//! [`SlotMap`]: values filed under keys the map issues, each key carrying the
//! generation of its slot, so a key that outlived its value is refused rather
//! than resolving to whatever took the slot next.

use alloc::vec::Vec;
use core::fmt;
use core::marker::PhantomData;
use core::num::NonZeroU32;

use crate::TryReserveError;

/// The end of the vacant chain, and the one index no slot is given.
const NONE: u32 = u32::MAX;

/// A slot's index and the generation of the value filed in it.
///
/// A key names a slot of the map that issued it. Presented to another map it
/// may name something there, so a holder that keeps several maps and must
/// tell their keys apart carries its own identity beside the key.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SlotKey {
    index: u32,
    generation: NonZeroU32,
}

impl SlotKey {
    /// The slot's index: dense from zero, so a holder can key a side table on
    /// it, and shared over time by every value the slot holds.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index as usize
    }

    /// Which of the slot's values this key names.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation.get()
    }
}

/// A key type a [`SlotMap`] issues: a [`SlotKey`] under a holder's own name,
/// so the keys of two maps cannot be confused at compile time.
///
/// A key is an identifier, not an authority: one made from an arbitrary
/// [`SlotKey`] is simply checked against the map like any other.
pub trait Key: Copy + Eq {
    /// The key naming `slot`.
    fn from_slot(slot: SlotKey) -> Self;

    /// The slot this key names.
    fn slot(self) -> SlotKey;
}

impl Key for SlotKey {
    fn from_slot(slot: SlotKey) -> Self {
        slot
    }

    fn slot(self) -> SlotKey {
        self
    }
}

enum Entry<V> {
    Occupied(V),
    Vacant {
        next: u32,
    },
    /// Its generation is spent, so no later value may take it.
    Retired,
}

struct Slot<V> {
    /// The generation of the value it holds, or of the next it will hold.
    generation: NonZeroU32,
    entry: Entry<V>,
}

/// The chain of vacant slots, most recently vacated first.
struct Vacancies {
    head: u32,
    len: usize,
}

impl<V> Slot<V> {
    /// Empty this slot if it is occupied, advancing its generation, and file
    /// it with `vacant` unless the generation is spent.
    fn vacate(&mut self, index: u32, vacant: &mut Vacancies) -> Option<V> {
        let next = self.generation.checked_add(1);
        let entry = match next {
            Some(_) => Entry::Vacant { next: vacant.head },
            None => Entry::Retired,
        };
        if !matches!(self.entry, Entry::Occupied(_)) {
            return None;
        }
        let Entry::Occupied(value) = core::mem::replace(&mut self.entry, entry) else {
            return None;
        };
        if let Some(next) = next {
            self.generation = next;
            vacant.head = index;
            vacant.len += 1;
        }
        Some(value)
    }
}

/// Values filed under keys the map issues, with O(1) insertion, lookup and
/// removal.
///
/// Each removal advances its slot's generation, so every key issued for the
/// slot before is refused from then on. A slot whose generation is spent is
/// retired rather than reused: no key is ever accepted for a value it did not
/// name, however long the map runs.
///
/// Vacant slots are reused most recently freed first, and iteration visits
/// slots in index order.
pub struct SlotMap<K, V> {
    slots: Vec<Slot<V>>,
    vacant: Vacancies,
    len: usize,
    key: PhantomData<fn() -> K>,
}

impl<K: Key, V> SlotMap<K, V> {
    /// An empty map, holding no storage.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            vacant: Vacancies { head: NONE, len: 0 },
            len: 0,
            key: PhantomData,
        }
    }

    /// An empty map with room for `capacity` values.
    ///
    /// # Errors
    ///
    /// As [`try_reserve`](Self::try_reserve).
    pub fn try_with_capacity(capacity: usize) -> Result<Self, TryReserveError> {
        let mut map = Self::new();
        map.try_reserve(capacity)?;
        Ok(map)
    }

    /// How many values the map holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the map holds no value.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How many values the map can hold before an insertion allocates.
    #[must_use]
    pub fn capacity(&self) -> usize {
        let unused = self.slots.capacity() - self.slots.len();
        self.len + self.vacant.len + unused.min(fresh_room(self.slots.len()))
    }

    /// Make room for `additional` more values, so that many insertions
    /// allocate nothing.
    ///
    /// # Errors
    ///
    /// [`TryReserveError::CapacityOverflow`] when the slots would need an index
    /// past the last one a key can carry, and [`TryReserveError::AllocFailed`]
    /// when the storage cannot be allocated. Nothing changes in either case.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        let fresh = additional.saturating_sub(self.vacant.len);
        if fresh == 0 {
            return Ok(());
        }
        if fresh > fresh_room(self.slots.len()) {
            return Err(TryReserveError::CapacityOverflow);
        }
        self.slots
            .try_reserve(fresh)
            .map_err(|_| TryReserveError::AllocFailed)
    }

    /// File `value`, answering the key it is filed under.
    ///
    /// # Errors
    ///
    /// As [`try_reserve`](Self::try_reserve); `value` is dropped.
    pub fn try_insert(&mut self, value: V) -> Result<K, TryReserveError> {
        self.try_insert_with_key(|_| value)
    }

    /// File the value `make` builds from the key it will be filed under.
    ///
    /// # Errors
    ///
    /// As [`try_reserve`](Self::try_reserve), and `make` is not called.
    pub fn try_insert_with_key(&mut self, make: impl FnOnce(K) -> V) -> Result<K, TryReserveError> {
        let head = self.vacant.head;
        if let Some(slot) = self.slots.get_mut(head as usize) {
            if let Entry::Vacant { next } = slot.entry {
                let key = key_at(head, slot.generation);
                slot.entry = Entry::Occupied(make(key));
                self.vacant.head = next;
                self.vacant.len -= 1;
                self.len += 1;
                return Ok(key);
            }
        }
        if fresh_room(self.slots.len()) == 0 {
            return Err(TryReserveError::CapacityOverflow);
        }
        let index =
            u32::try_from(self.slots.len()).map_err(|_| TryReserveError::CapacityOverflow)?;
        self.slots
            .try_reserve(1)
            .map_err(|_| TryReserveError::AllocFailed)?;
        let key = key_at(index, NonZeroU32::MIN);
        self.slots.push(Slot {
            generation: NonZeroU32::MIN,
            entry: Entry::Occupied(make(key)),
        });
        self.len += 1;
        Ok(key)
    }

    /// The value `key` names, while it is filed.
    #[must_use]
    pub fn get(&self, key: K) -> Option<&V> {
        let key = key.slot();
        match self.slots.get(key.index()) {
            Some(Slot {
                generation,
                entry: Entry::Occupied(value),
            }) if *generation == key.generation => Some(value),
            _ => None,
        }
    }

    /// The value `key` names, while it is filed.
    #[must_use]
    pub fn get_mut(&mut self, key: K) -> Option<&mut V> {
        let key = key.slot();
        match self.slots.get_mut(key.index()) {
            Some(Slot {
                generation,
                entry: Entry::Occupied(value),
            }) if *generation == key.generation => Some(value),
            _ => None,
        }
    }

    /// Whether `key` names a filed value.
    #[must_use]
    pub fn contains_key(&self, key: K) -> bool {
        self.get(key).is_some()
    }

    /// Take out the value `key` names, refusing every key issued for it from
    /// then on.
    pub fn remove(&mut self, key: K) -> Option<V> {
        let key = key.slot();
        let slot = self.slots.get_mut(key.index())?;
        if slot.generation != key.generation {
            return None;
        }
        let value = slot.vacate(key.index, &mut self.vacant)?;
        self.len -= 1;
        Some(value)
    }

    /// Keep only the values `keep` answers `true` for.
    pub fn retain(&mut self, mut keep: impl FnMut(K, &mut V) -> bool) {
        let Self {
            slots, vacant, len, ..
        } = self;
        for (index, slot) in (0..NONE).zip(slots.iter_mut()) {
            let Entry::Occupied(value) = &mut slot.entry else {
                continue;
            };
            if !keep(key_at(index, slot.generation), value) && slot.vacate(index, vacant).is_some()
            {
                *len -= 1;
            }
        }
    }

    /// Remove every value. Their slots stay, at advanced generations, so
    /// every key issued before is still refused.
    pub fn clear(&mut self) {
        self.retain(|_, _| false);
    }

    /// Each filed value and its key, in slot order.
    pub fn iter(&self) -> impl Iterator<Item = (K, &V)> {
        (0..NONE)
            .zip(self.slots.iter())
            .filter_map(|(index, slot)| match &slot.entry {
                Entry::Occupied(value) => Some((key_at(index, slot.generation), value)),
                _ => None,
            })
    }

    /// Each filed value, mutably, and its key, in slot order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (K, &mut V)> {
        (0..NONE)
            .zip(self.slots.iter_mut())
            .filter_map(|(index, slot)| match &mut slot.entry {
                Entry::Occupied(value) => Some((key_at(index, slot.generation), value)),
                _ => None,
            })
    }

    /// Each filed value's key, in slot order.
    pub fn keys(&self) -> impl Iterator<Item = K> + '_ {
        self.iter().map(|(key, _)| key)
    }

    /// Each filed value, in slot order.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }

    /// Each filed value, mutably, in slot order.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.iter_mut().map(|(_, value)| value)
    }
}

/// How many slots can still be appended after `len`: a key's index stops one
/// short of [`NONE`].
const fn fresh_room(len: usize) -> usize {
    (NONE as usize).saturating_sub(len)
}

fn key_at<K: Key>(index: u32, generation: NonZeroU32) -> K {
    K::from_slot(SlotKey { index, generation })
}

impl<K: Key, V> Default for SlotMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Key + fmt::Debug, V: fmt::Debug> fmt::Debug for SlotMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

#[cfg(test)]
#[path = "slotmap_tests.rs"]
mod tests;
