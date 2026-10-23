//! The address-space tags one CPU gives the user spaces it runs, and when a
//! tag's entries are stale.
//!
//! A tag is current while it holds the same space at the generation and
//! kernel epoch it last caught up with: when it was discarded, or when this CPU
//! invalidated the space itself under it. A CPU's switch into a space loads it
//! under its tag, discarding the tag first only when it was another space's or
//! has gone stale; the tag left longest unloaded is the one given to a space
//! that has none. Tag `0` is the park root's.

use tairix_arch_api::{AddressTag, AddressTags, MAX_USER_TAGS};

use crate::retire::TlbContext;

/// One CPU's tags.
#[derive(Debug)]
pub struct TagSlots {
    slots: [Slot; MAX_USER_TAGS],
    park: Park,
    /// Advanced by every load, so the least recently loaded tag has the
    /// lowest stamp and a tag never loaded has nought.
    clock: u64,
    /// The space whose root is in force under its tag, `0` for none: from the
    /// recheck after its load until the next switch or park. Only then does a
    /// local invalidation of it reach its tag.
    running: u64,
}

/// What one user tag holds.
#[derive(Copy, Clone, Debug, Default)]
struct Slot {
    /// The space's id, `0` for none: no space is id `0`.
    space: u64,
    generation: u64,
    epoch: u64,
    loaded: u64,
}

/// What the park root's tag holds.
#[derive(Copy, Clone, Debug, Default)]
struct Park {
    epoch: u64,
    /// A user root was loaded under tag `0` since it was last discarded.
    dirty: bool,
}

/// A space's generation and the kernel epoch, as far as the port's tags need
/// them.
struct Seen {
    generation: u64,
    epoch: u64,
}

impl Seen {
    fn now(space: &TlbContext, tags: &dyn AddressTags) -> Self {
        Self {
            // A port whose invalidations reach every tag leaves no CPU a
            // stale entry to catch up on.
            generation: if tags.invalidation_reaches_every_tag() {
                0
            } else {
                space.generation()
            },
            epoch: tags.kernel_epoch(),
        }
    }
}

impl TagSlots {
    /// No user tag holding anything, and tag `0` owed a discard: what boot
    /// ran under it before the table existed is not known.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [Slot {
                space: 0,
                generation: 0,
                epoch: 0,
                loaded: 0,
            }; MAX_USER_TAGS],
            park: Park {
                epoch: 0,
                dirty: true,
            },
            clock: 0,
            running: 0,
        }
    }

    /// The tag `space` is loaded under on this CPU, read after the CPU entered
    /// the space's set, and whether the tag's entries must go first.
    pub fn tag(&mut self, space: &TlbContext, tags: &dyn AddressTags) -> AddressTag {
        self.running = 0;
        let count = Self::count(tags);
        if count == 0 {
            return self.untagged();
        }
        let seen = Seen::now(space, tags);
        self.clock = self.clock.saturating_add(1);
        let slots = &mut self.slots[..count];
        let held = slots.iter().position(|slot| slot.space == space.id());
        let index = held.unwrap_or_else(|| {
            slots
                .iter()
                .enumerate()
                .min_by_key(|(_, slot)| slot.loaded)
                .map_or(0, |(index, _)| index)
        });
        let slot = &mut slots[index];
        let flush =
            held.is_none() || slot.generation != seen.generation || slot.epoch != seen.epoch;
        *slot = Slot {
            space: space.id(),
            generation: seen.generation,
            epoch: seen.epoch,
            loaded: self.clock,
        };
        AddressTag {
            tag: u16::try_from(index + 1).unwrap_or(u16::MAX),
            flush,
        }
    }

    /// How a root with no standing with the TLBs is loaded: under tag `0`,
    /// discarded first, so the park after it discards it again.
    pub fn untagged(&mut self) -> AddressTag {
        self.running = 0;
        self.park.dirty = true;
        AddressTag::UNTAGGED
    }

    /// Keep `space`'s tag current across the invalidation that advanced its
    /// generation from `from`, which this CPU made itself with the space's
    /// root in force under that tag, discarding its entries as it did: current
    /// with every invalidation before it, the tag is current with this one
    /// too. A CPU that has entered the space but not yet loaded it, or has
    /// parked, discarded under another tag, and earns nothing.
    pub fn credit(&mut self, space: &TlbContext, from: u64, tags: &dyn AddressTags) {
        if tags.invalidation_reaches_every_tag() || self.running != space.id() {
            return;
        }
        let count = Self::count(tags);
        if let Some(slot) = self.slots[..count]
            .iter_mut()
            .find(|slot| slot.space == space.id() && slot.generation == from)
        {
            slot.generation = from.wrapping_add(1);
        }
    }

    /// Whether `tag`, just loaded for `space`, must be discarded again: an
    /// invalidation raced the switch, and may have reached this CPU only
    /// under the tag it left. From here the space runs under its tag.
    pub fn recheck(&mut self, tag: AddressTag, space: &TlbContext, tags: &dyn AddressTags) -> bool {
        let Some(slot) = usize::from(tag.tag)
            .checked_sub(1)
            .and_then(|index| self.slots.get_mut(index))
            .filter(|slot| slot.space == space.id())
        else {
            return false;
        };
        let seen = Seen::now(space, tags);
        let stale = slot.generation != seen.generation || slot.epoch != seen.epoch;
        slot.generation = seen.generation;
        slot.epoch = seen.epoch;
        self.running = space.id();
        stale
    }

    /// Whether tag `0` must be discarded before the park root is loaded under
    /// it: always on a CPU that tags nothing, else when a user root used it or
    /// the kernel epoch moved since it was.
    pub fn park(&mut self, tags: &dyn AddressTags) -> bool {
        self.running = 0;
        if Self::count(tags) == 0 {
            return true;
        }
        let epoch = tags.kernel_epoch();
        let flush = core::mem::take(&mut self.park.dirty) || self.park.epoch != epoch;
        self.park.epoch = epoch;
        flush
    }

    /// A park that did not load the park root: a discard it owed is owed
    /// still.
    pub fn park_refused(&mut self, flush: bool) {
        self.park.dirty |= flush;
    }

    /// Whether tag `0`, just loaded, must be discarded again: a kernel
    /// invalidation raced the park.
    pub fn recheck_park(&mut self, tags: &dyn AddressTags) -> bool {
        if Self::count(tags) == 0 {
            return false;
        }
        let epoch = tags.kernel_epoch();
        let stale = self.park.epoch != epoch;
        self.park.epoch = epoch;
        stale
    }

    /// The user tags this CPU gives out.
    fn count(tags: &dyn AddressTags) -> usize {
        usize::from(tags.user_tags()).min(MAX_USER_TAGS)
    }
}

impl Default for TagSlots {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "tags_tests.rs"]
mod tests;
