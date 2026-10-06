//! What each stream of a unit translates through, and how many streams hold
//! each domain: the bookkeeping every family's attach, block and silence
//! share, so the rule that keeps a domain's tables alive is written once.

use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;

use crate::IommuError;

/// What one stream's entry translates through.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Binding {
    /// The domain of this id.
    Domain(u32),
    /// Nothing, and its faults go unrecorded.
    Silenced,
}

/// Room for one binding, reserved before the writes that make it, so
/// recording the binding once the unit may act on it cannot fail.
#[must_use]
pub struct Room(());

/// One stream's entry, and the domain the unit may still walk for it.
#[derive(Copy, Clone, Debug, Default)]
struct Slot {
    /// What the entry does; [`None`] blocks the stream.
    entry: Option<Binding>,
    /// The domain the entry names, or one an entry since written stopped
    /// naming, until the unit confirms it forgot it.
    held: Option<u32>,
}

/// Each stream's binding, and each domain's holders.
///
/// A stream holds its domain from the moment the unit may walk the domain's
/// tables for it until the unit confirms it forgot them: a domain is never
/// destroyed under a walk the unit could still make, and a block the unit
/// could not confirm leaves the stream holding it for a later block to
/// confirm. Silence holds nothing, so it ends as soon as it is overwritten.
pub struct Bindings {
    streams: HashMap<u32, Slot, BuildFastHash>,
    holders: HashMap<u32, usize, BuildFastHash>,
}

impl Default for Bindings {
    fn default() -> Self {
        Self::new()
    }
}

impl Bindings {
    /// No stream bound.
    #[must_use]
    pub fn new() -> Self {
        Self {
            streams: HashMap::with_hasher(BuildFastHash::new()),
            holders: HashMap::with_hasher(BuildFastHash::new()),
        }
    }

    /// What `stream`'s entry translates through.
    #[must_use]
    pub fn get(&self, stream: u32) -> Option<Binding> {
        self.streams.get(&stream).and_then(|slot| slot.entry)
    }

    /// The domain `stream` holds, through its entry or one the unit has not
    /// confirmed it forgot.
    #[must_use]
    pub fn held(&self, stream: u32) -> Option<u32> {
        self.streams.get(&stream).and_then(|slot| slot.held)
    }

    /// Whether `stream` holds a domain.
    #[must_use]
    pub fn holds_domain(&self, stream: u32) -> bool {
        self.held(stream).is_some()
    }

    /// The room to attach `stream` to `domain`, or [`None`] where its entry
    /// already translates through it.
    ///
    /// # Errors
    ///
    /// [`IommuError::StreamBusy`] where it holds another domain, and
    /// [`IommuError::Exhausted`] when the room cannot be had.
    pub fn prepare_attach(&mut self, stream: u32, domain: u32) -> Result<Option<Room>, IommuError> {
        let slot = self.streams.get(&stream).copied().unwrap_or_default();
        if slot.entry == Some(Binding::Domain(domain)) {
            return Ok(None);
        }
        if slot.held.is_some_and(|held| held != domain) {
            return Err(IommuError::StreamBusy);
        }
        self.reserve().map(Some)
    }

    /// Room for one more binding.
    ///
    /// # Errors
    ///
    /// [`IommuError::Exhausted`] when it cannot be had.
    pub fn reserve(&mut self) -> Result<Room, IommuError> {
        self.streams
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        self.holders
            .try_reserve(1)
            .map_err(|_| IommuError::Exhausted)?;
        Ok(Room(()))
    }

    /// From now on the unit may walk `domain`'s tables for `stream`.
    pub fn hold(&mut self, _room: Room, stream: u32, domain: u32) {
        let mut slot = self.streams.get(&stream).copied().unwrap_or_default();
        match slot.held {
            Some(held) if held != domain => return,
            Some(_) => {}
            None => match self.holders.get_mut(&domain) {
                Some(count) => *count += 1,
                None => {
                    let _ = self.holders.try_insert(domain, 1);
                }
            },
        }
        slot.entry = Some(Binding::Domain(domain));
        slot.held = Some(domain);
        let _ = self.streams.try_insert(stream, slot);
    }

    /// `stream`'s entry no longer translates through its domain, which it
    /// still holds until [`Self::release`].
    pub fn unbind(&mut self, stream: u32) {
        if let Some(slot) = self.streams.get_mut(&stream) {
            if matches!(slot.entry, Some(Binding::Domain(_))) {
                slot.entry = None;
            }
        }
    }

    /// From now on `stream` is silent. A domain it still holds it keeps
    /// holding until [`Self::release`].
    pub fn silence(&mut self, _room: Room, stream: u32) {
        let mut slot = self.streams.get(&stream).copied().unwrap_or_default();
        slot.entry = Some(Binding::Silenced);
        let _ = self.streams.try_insert(stream, slot);
    }

    /// `stream`'s silence ended with its configuration overwritten.
    pub fn end_silence(&mut self, stream: u32) {
        if let Some(slot) = self.streams.get_mut(&stream) {
            if slot.entry == Some(Binding::Silenced) {
                slot.entry = None;
            }
        }
        self.forget_idle(stream);
    }

    /// The unit confirmed it forgot every configuration `stream` had but
    /// its current one: a domain the entry no longer names is let go, and
    /// one it still names is let go only with the entry, as by a confirmed
    /// block.
    pub fn release(&mut self, stream: u32) {
        let Some(slot) = self.streams.get_mut(&stream) else {
            return;
        };
        if matches!(slot.entry, Some(Binding::Domain(_))) {
            slot.entry = None;
        }
        if let Some(domain) = slot.held.take() {
            if let Some(count) = self.holders.get_mut(&domain) {
                *count -= 1;
                if *count == 0 {
                    self.holders.remove(&domain);
                }
            }
        }
        self.forget_idle(stream);
    }

    /// Streams holding `domain`.
    #[must_use]
    pub fn holders(&self, domain: u32) -> usize {
        self.holders.get(&domain).copied().unwrap_or(0)
    }

    /// Drop `stream`'s slot once it records nothing.
    fn forget_idle(&mut self, stream: u32) {
        if self
            .streams
            .get(&stream)
            .is_some_and(|slot| slot.entry.is_none() && slot.held.is_none())
        {
            self.streams.remove(&stream);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_domain_is_held_from_attach_until_each_holder_is_released() {
        let mut bindings = Bindings::new();
        for stream in [1, 2] {
            let room = bindings.prepare_attach(stream, 7).unwrap().unwrap();
            bindings.hold(room, stream, 7);
        }
        assert_eq!(bindings.holders(7), 2);
        assert!(bindings.prepare_attach(1, 7).unwrap().is_none());
        assert!(matches!(
            bindings.prepare_attach(1, 8),
            Err(IommuError::StreamBusy)
        ));
        bindings.release(1);
        assert_eq!(bindings.holders(7), 1);
        bindings.release(2);
        assert_eq!(bindings.holders(7), 0);
        assert_eq!(bindings.get(2), None);
    }

    #[test]
    fn silence_holds_nothing_and_never_hides_a_held_domain() {
        let mut bindings = Bindings::new();
        let room = bindings.reserve().unwrap();
        bindings.silence(room, 3);
        assert_eq!(bindings.get(3), Some(Binding::Silenced));
        assert!(!bindings.holds_domain(3));
        bindings.end_silence(3);
        assert_eq!(bindings.get(3), None);
        let room = bindings.prepare_attach(3, 9).unwrap().unwrap();
        bindings.hold(room, 3, 9);
        let room = bindings.reserve().unwrap();
        bindings.silence(room, 3);
        bindings.end_silence(3);
        assert_eq!(bindings.get(3), None);
        assert!(bindings.holds_domain(3), "only a release lets a domain go");
        assert_eq!(bindings.holders(9), 1);
        bindings.release(3);
        assert_eq!(bindings.holders(9), 0);
        assert!(!bindings.holds_domain(3));
    }

    /// An entry cleared without the unit's confirmation still holds its
    /// domain: attaching it there again rewrites the entry without a second
    /// hold, and attaching it anywhere else waits for the release.
    #[test]
    fn an_unconfirmed_block_holds_its_domain_and_its_reattach_rewrites_the_entry() {
        let mut bindings = Bindings::new();
        let room = bindings.prepare_attach(3, 9).unwrap().unwrap();
        bindings.hold(room, 3, 9);
        bindings.unbind(3);
        assert_eq!(bindings.get(3), None);
        assert!(bindings.holds_domain(3));
        assert!(matches!(
            bindings.prepare_attach(3, 8),
            Err(IommuError::StreamBusy)
        ));
        let room = bindings
            .prepare_attach(3, 9)
            .unwrap()
            .expect("a cleared entry is written again");
        bindings.hold(room, 3, 9);
        assert_eq!(bindings.get(3), Some(Binding::Domain(9)));
        assert_eq!(bindings.holders(9), 1, "one stream, one hold");
        bindings.unbind(3);
        bindings.release(3);
        assert_eq!(bindings.holders(9), 0);
        assert!(bindings.prepare_attach(3, 8).unwrap().is_some());
    }
}
