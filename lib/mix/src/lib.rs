//! Integer finalisers: a word hashed to a well-spread word of its own width.
//!
//! [`mix64`] is `SplitMix64`'s output function and [`SplitMix64`] the generator
//! it finalises (Steele, Lea and Flood, "Fast Splittable Pseudorandom Number
//! Generators", 2014); [`mix32`] is Wellons's lowbias32. Each is a bijection
//! with full avalanche. None is keyed or cryptographic: they spread counters,
//! lattice places and seeds, never a secret, and an input chosen to collide
//! collides.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// `SplitMix64`'s Weyl increment: 2⁶⁴ over the golden ratio, odd.
pub const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// `SplitMix64`'s output finaliser.
#[must_use]
#[inline]
pub const fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Wellons's lowbias32 finaliser.
#[must_use]
#[inline]
pub const fn mix32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^ (x >> 16)
}

/// The key after `key` in a chain of keys, each drawn from the last: the
/// last stepped by the 32-bit golden-ratio increment and finalised.
#[must_use]
#[inline]
pub const fn next_key(key: u32) -> u32 {
    mix32(key ^ 0x9E37_79B9)
}

/// The key of the `index`th of the streams keyed from `seed`.
#[must_use]
#[inline]
pub const fn numbered(seed: u64, index: u64) -> u64 {
    mix64(seed ^ mix64(index.wrapping_add(1)))
}

/// `bits` as a fraction in `0.0..1.0`.
#[must_use]
#[inline]
pub fn unit(bits: u32) -> f64 {
    f64::from(bits) * (1.0 / 4_294_967_296.0)
}

/// `SplitMix64`: a Weyl sequence through [`mix64`]. Not `Copy`, so a stream
/// handed on by value is moved, never silently forked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// The generator whose first word is drawn from `seed`.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Its next word.
    #[inline]
    pub const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GOLDEN_GAMMA);
        mix64(self.state)
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
