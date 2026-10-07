//! Draws keyed by what they are for and where, never by the order they are
//! made in: a layout laid out in pieces draws exactly what one laid out whole
//! does.

use core::hash::Hasher;

use tairix_hash::FastHash;
use tairix_rng::rand::unit_from;

/// A layout's key: the one seed every draw is keyed from.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Key(u64);

impl Key {
    /// The key of `seed`.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The draws `stage` makes at `place`.
    pub(crate) const fn draws(self, stage: Stage, place: (i64, i64)) -> Draws {
        Draws {
            key: self,
            stage,
            place,
            next: 0,
        }
    }

    /// The draws `stage` makes for what the word `identity` names, where a
    /// place's two coordinates cannot name it.
    pub(crate) const fn draws_for(self, stage: Stage, identity: u64) -> Draws {
        self.draws(stage, (i64::from_le_bytes(identity.to_le_bytes()), 0))
    }

    /// A hasher keyed for `stage`, which an identity is written into field by
    /// field, each as little-endian bytes, so its word is the same on every
    /// target.
    pub(crate) fn hasher(self, stage: Stage) -> FastHash {
        let mut hasher = FastHash::with_seed(self.0);
        hasher.write_u32(stage as u32);
        hasher
    }

    /// The `index`th word `stage` draws at `place`.
    pub(crate) fn word(self, stage: Stage, (x, y): (i64, i64), index: u32) -> u64 {
        let mut message = [0u8; 24];
        message[0..4].copy_from_slice(&(stage as u32).to_le_bytes());
        message[4..8].copy_from_slice(&index.to_le_bytes());
        message[8..16].copy_from_slice(&x.to_le_bytes());
        message[16..24].copy_from_slice(&y.to_le_bytes());
        FastHash::hash_bytes(self.0, &message)
    }
}

/// What a draw is for, so the draws two purposes make at one place never
/// coincide. Each value is its own for good: a new purpose takes a new one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub(crate) enum Stage {
    Holding = 1,
    Farmstead = 2,
    Village = 4,
    Gateway = 5,
    Route = 6,
    Cut = 7,
    Boundary = 8,
    Gate = 9,
    Use = 10,
    Yard = 11,
    Plot = 12,
    Custom = 13,
    Way = 14,
}

/// The draws one purpose makes at one place, each a word of its own: drawn
/// in a fixed order at that place, but owing nothing to any other place's.
#[derive(Debug)]
pub(crate) struct Draws {
    key: Key,
    stage: Stage,
    place: (i64, i64),
    next: u32,
}

impl Draws {
    /// The next word.
    pub(crate) fn word(&mut self) -> u64 {
        let word = self.key.word(self.stage, self.place, self.next);
        self.next = self.next.wrapping_add(1);
        word
    }

    /// The next draw in `0.0..1.0`.
    pub(crate) fn unit(&mut self) -> f64 {
        unit_from(self.word())
    }

    /// The next draw in `low..high`.
    pub(crate) fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    /// One of `weights`' choices, each as likely as its weight against the
    /// others' — a weight below nought counting as nought — from the next
    /// draw; `None` where every weight is nought.
    pub(crate) fn pick<T: Copy>(&mut self, weights: &[(T, f64)]) -> Option<T> {
        let total: f64 = weights.iter().map(|&(_, weight)| weight.max(0.0)).sum();
        let mut drawn = self.unit() * total;
        if !total.is_finite() || total <= 0.0 {
            return None;
        }
        for &(choice, weight) in weights {
            drawn -= weight.max(0.0);
            if drawn < 0.0 {
                return Some(choice);
            }
        }
        weights
            .iter()
            .rev()
            .find(|&&(_, weight)| weight > 0.0)
            .map(|&(choice, _)| choice)
    }

    /// Whether the next draw falls within `chance`.
    pub(crate) fn chance(&mut self, chance: f64) -> bool {
        self.unit() < chance
    }

    /// The next draw in `0..count`, or nought for none: rejection-free, so
    /// every place takes the same number of words however its draws fall.
    pub(crate) fn below(&mut self, count: usize) -> usize {
        let count = u64::try_from(count).unwrap_or(u64::MAX).max(1);
        usize::try_from(self.word() % count).unwrap_or(0)
    }
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
