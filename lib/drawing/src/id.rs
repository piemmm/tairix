//! Object identities.

use core::num::NonZeroU64;

/// An object's identity, unique within the drawing that issued it and never
/// reused there: what a selection, a history step and a worker's answer name
/// an object by, however the tree around it changes.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ObjectId(NonZeroU64);

impl ObjectId {
    /// The identity's number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Issues identities, each once.
#[derive(Clone, Debug)]
pub struct Ids {
    next: u64,
}

impl Default for Ids {
    fn default() -> Self {
        Self::new()
    }
}

impl Ids {
    /// An issuer that has issued nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self { next: 1 }
    }

    /// A fresh identity.
    ///
    /// Sixty-four bits issued one at a time do not run out; were they to,
    /// the last is issued again rather than wrapping to one already held.
    pub fn issue(&mut self) -> ObjectId {
        let issued = NonZeroU64::new(self.next).unwrap_or(NonZeroU64::MAX);
        self.next = self.next.saturating_add(1);
        ObjectId(issued)
    }
}

#[cfg(test)]
mod tests {
    use super::Ids;

    #[test]
    fn every_identity_is_fresh() {
        let mut ids = Ids::new();
        let a = ids.issue();
        let b = ids.issue();
        assert_ne!(a, b);
        assert!(b > a);
        assert_eq!(a.get(), 1);
    }
}
